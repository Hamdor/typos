pub struct ConfigStorage {
    arena: std::sync::Mutex<typed_arena::Arena<kstring::KString>>,
}

impl ConfigStorage {
    pub fn new() -> Self {
        Self {
            arena: std::sync::Mutex::new(typed_arena::Arena::new()),
        }
    }

    fn get<'s>(&'s self, other: &str) -> &'s str {
        // Safe because we the references are stable once created.
        //
        // Trying to get this handled inside of `typed_arena` directly, see
        // https://github.com/SimonSapin/rust-typed-arena/issues/49#issuecomment-809517312
        unsafe {
            std::mem::transmute::<&str, &str>(
                self.arena
                    .lock()
                    .unwrap()
                    .alloc(kstring::KString::from_ref(other))
                    .as_str(),
            )
        }
    }
}

impl Default for ConfigStorage {
    fn default() -> Self {
        Self::new()
    }
}

pub struct ConfigEngine<'s> {
    storage: &'s ConfigStorage,

    overrides: Option<crate::config::Config>,
    isolated: bool,

    configs: std::collections::HashMap<std::path::PathBuf, DirConfig>,
    walk: Intern<crate::config::Walk>,
    tokenizer: Intern<typos::tokens::Tokenizer>,
    dict: Intern<crate::dict::Override<'s, 's, crate::dict::BuiltIn>>,
}

impl<'s> ConfigEngine<'s> {
    pub fn new(storage: &'s ConfigStorage) -> Self {
        Self {
            storage,
            overrides: Default::default(),
            configs: Default::default(),
            isolated: false,
            walk: Default::default(),
            tokenizer: Default::default(),
            dict: Default::default(),
        }
    }

    pub fn set_overrides(&mut self, overrides: crate::config::Config) -> &mut Self {
        self.overrides = Some(overrides);
        self
    }

    pub fn set_isolated(&mut self, isolated: bool) -> &mut Self {
        self.isolated = isolated;
        self
    }

    pub fn walk(&self, cwd: &std::path::Path) -> &crate::config::Walk {
        debug_assert!(cwd.is_absolute(), "{} is not absolute", cwd.display());
        let dir = self
            .configs
            .get(cwd)
            .expect("`init_dir` must be called first");
        self.get_walk(dir)
    }

    pub fn file_types(&self, cwd: &std::path::Path) -> &[ignore::types::FileTypeDef] {
        debug_assert!(cwd.is_absolute(), "{} is not absolute", cwd.display());
        let dir = self
            .configs
            .get(cwd)
            .expect("`init_dir` must be called first");
        dir.type_matcher.definitions()
    }

    pub fn policy(&self, path: &std::path::Path) -> Policy<'_, '_> {
        debug_assert!(path.is_absolute(), "{} is not absolute", path.display());
        let dir = self.get_dir(path).expect("`walk()` should be called first");
        let file_config = dir.get_file_config(path);
        Policy {
            check_filenames: file_config.check_filenames,
            check_files: file_config.check_files,
            binary: file_config.binary,
            tokenizer: self.get_tokenizer(&file_config),
            dict: self.get_dict(&file_config),
        }
    }

    fn get_walk(&self, dir: &DirConfig) -> &crate::config::Walk {
        self.walk.get(dir.walk)
    }

    fn get_tokenizer(&self, file: &FileConfig) -> &typos::tokens::Tokenizer {
        self.tokenizer.get(file.tokenizer)
    }

    fn get_dict(&self, file: &FileConfig) -> &dyn typos::Dictionary {
        self.dict.get(file.dict)
    }

    fn get_dir(&self, path: &std::path::Path) -> Option<&DirConfig> {
        for path in path.ancestors() {
            if let Some(dir) = self.configs.get(path) {
                return Some(dir);
            }
        }
        None
    }

    pub fn load_config(
        &self,
        cwd: &std::path::Path,
    ) -> Result<crate::config::Config, anyhow::Error> {
        debug_assert!(cwd.is_absolute(), "{} is not absolute", cwd.display());
        let mut config = crate::config::Config::default();

        if !self.isolated {
            for ancestor in cwd.ancestors() {
                if let Some(derived) = crate::config::Config::from_dir(ancestor)? {
                    config.update(&derived);
                    break;
                }
            }
        }
        if let Some(overrides) = self.overrides.as_ref() {
            config.update(overrides);
        }

        let mut types = Default::default();
        std::mem::swap(&mut types, &mut config.type_);
        let mut types = types
            .into_iter()
            .map(|(type_, type_engine)| {
                let mut new_engine = config.default.clone();
                new_engine.update(&type_engine.engine);
                new_engine.update(&config.overrides);
                let new_type_engine = crate::config::TypeEngineConfig {
                    extend_glob: type_engine.extend_glob,
                    engine: new_engine,
                };
                (type_, new_type_engine)
            })
            .collect();
        std::mem::swap(&mut types, &mut config.type_);

        config.default.update(&config.overrides);

        Ok(config)
    }

    pub fn init_dir(&mut self, cwd: &std::path::Path) -> Result<(), anyhow::Error> {
        debug_assert!(cwd.is_absolute(), "{} is not absolute", cwd.display());
        if self.configs.contains_key(cwd) {
            return Ok(());
        }

        let config = self.load_config(cwd)?;
        let crate::config::Config {
            files,
            mut default,
            type_,
            overrides,
        } = config;

        let walk = self.walk.intern(files);

        let mut type_matcher = ignore::types::TypesBuilder::new();
        type_matcher.add_defaults();
        let mut types: std::collections::HashMap<_, _> = Default::default();
        for (type_name, type_engine) in type_.into_iter() {
            if type_engine.extend_glob.is_empty() {
                if type_matcher
                    .definitions()
                    .iter()
                    .all(|def| def.name() != type_name.as_str())
                {
                    anyhow::bail!("Unknown type definition `{}`, pass `--type-list` to see valid names or set `extend_glob` to add a new one.", type_name);
                }
            } else {
                for glob in type_engine.extend_glob.iter() {
                    type_matcher.add(type_name.as_str(), glob.as_str())?;
                }
            }

            let type_config = self.init_file_config(type_engine.engine);
            types.insert(type_name, type_config);
        }
        default.update(&overrides);
        let default = self.init_file_config(default);

        type_matcher.select("all");

        let dir = DirConfig {
            walk,
            default,
            types,
            type_matcher: type_matcher.build()?,
        };

        self.configs.insert(cwd.to_owned(), dir);
        Ok(())
    }

    fn init_file_config(&mut self, engine: crate::config::EngineConfig) -> FileConfig {
        let binary = engine.binary();
        let check_filename = engine.check_filename();
        let check_file = engine.check_file();
        let crate::config::EngineConfig {
            tokenizer, dict, ..
        } = engine;
        let tokenizer_config =
            tokenizer.unwrap_or_else(crate::config::TokenizerConfig::from_defaults);
        let dict_config = dict.unwrap_or_else(crate::config::DictConfig::from_defaults);

        let tokenizer = typos::tokens::TokenizerBuilder::new()
            .unicode(tokenizer_config.unicode())
            .ignore_hex(tokenizer_config.ignore_hex())
            .leading_digits(tokenizer_config.identifier_leading_digits())
            .build();

        let dict = crate::dict::BuiltIn::new(dict_config.locale());
        let mut dict = crate::dict::Override::new(dict);
        dict.identifiers(
            dict_config
                .extend_identifiers()
                .map(|(k, v)| (self.storage.get(k), self.storage.get(v))),
        );
        dict.words(
            dict_config
                .extend_words()
                .map(|(k, v)| (self.storage.get(k), self.storage.get(v))),
        );

        let dict = self.dict.intern(dict);
        let tokenizer = self.tokenizer.intern(tokenizer);

        FileConfig {
            check_filenames: check_filename,
            check_files: check_file,
            binary,
            tokenizer,
            dict,
        }
    }
}

struct Intern<T> {
    data: Vec<T>,
}

impl<T> Intern<T> {
    pub fn new() -> Self {
        Self {
            data: Default::default(),
        }
    }

    pub fn intern(&mut self, value: T) -> usize {
        let symbol = self.data.len();
        self.data.push(value);
        symbol
    }

    pub fn get(&self, symbol: usize) -> &T {
        &self.data[symbol]
    }
}

impl<T> Default for Intern<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug)]
struct DirConfig {
    walk: usize,
    default: FileConfig,
    types: std::collections::HashMap<kstring::KString, FileConfig>,
    type_matcher: ignore::types::Types,
}

impl DirConfig {
    fn get_file_config(&self, path: &std::path::Path) -> FileConfig {
        let match_ = self.type_matcher.matched(path, false);
        let name = match_
            .inner()
            .and_then(|g| g.file_type_def())
            .map(|f| f.name());

        name.and_then(|name| self.types.get(name).copied())
            .unwrap_or(self.default)
    }
}

#[derive(Copy, Clone, Debug)]
struct FileConfig {
    tokenizer: usize,
    dict: usize,
    check_filenames: bool,
    check_files: bool,
    binary: bool,
}

#[non_exhaustive]
#[derive(derive_setters::Setters)]
pub struct Policy<'t, 'd> {
    pub check_filenames: bool,
    pub check_files: bool,
    pub binary: bool,
    pub tokenizer: &'t typos::tokens::Tokenizer,
    pub dict: &'d dyn typos::Dictionary,
}

impl<'t, 'd> Policy<'t, 'd> {
    pub fn new() -> Self {
        Default::default()
    }
}

static DEFAULT_TOKENIZER: once_cell::sync::Lazy<typos::tokens::Tokenizer> =
    once_cell::sync::Lazy::new(typos::tokens::Tokenizer::new);
static DEFAULT_DICT: crate::dict::BuiltIn = crate::dict::BuiltIn::new(crate::config::Locale::En);

impl<'t, 'd> Default for Policy<'t, 'd> {
    fn default() -> Self {
        Self {
            check_filenames: true,
            check_files: true,
            binary: false,
            tokenizer: &DEFAULT_TOKENIZER,
            dict: &DEFAULT_DICT,
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    const NEVER_EXIST_TYPE: &str = "THISyTYPEySHOULDyNEVERyEXISTyBUTyIyHATEyYOUyIFyITyDOES";

    #[test]
    fn test_load_config_applies_overrides() {
        let storage = ConfigStorage::new();
        let mut engine = ConfigEngine::new(&storage);
        engine.set_isolated(true);

        let type_name = kstring::KString::from_static("toml");

        let config = crate::config::Config {
            default: crate::config::EngineConfig {
                binary: Some(true),
                check_filename: Some(true),
                ..Default::default()
            },
            type_: maplit::hashmap! {
                type_name.clone() => crate::config::TypeEngineConfig {
                    engine: crate::config::EngineConfig {
                        check_filename: Some(false),
                        check_file: Some(true),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            },
            overrides: crate::config::EngineConfig {
                binary: Some(false),
                check_file: Some(false),
                ..Default::default()
            },
            ..Default::default()
        };
        engine.set_overrides(config);

        let cwd = std::path::Path::new(".").canonicalize().unwrap();
        let loaded = engine.load_config(&cwd).unwrap();
        assert_eq!(loaded.default.binary, Some(false));
        assert_eq!(loaded.default.check_filename, Some(true));
        assert_eq!(loaded.default.check_file, Some(false));
        assert_eq!(loaded.type_[type_name.as_str()].engine.binary, Some(false));
        assert_eq!(
            loaded.type_[type_name.as_str()].engine.check_filename,
            Some(false)
        );
        assert_eq!(
            loaded.type_[type_name.as_str()].engine.check_file,
            Some(false)
        );
    }

    #[test]
    fn test_init_fails_on_unknown_type() {
        let storage = ConfigStorage::new();
        let mut engine = ConfigEngine::new(&storage);
        engine.set_isolated(true);

        let type_name = kstring::KString::from_static(NEVER_EXIST_TYPE);

        let config = crate::config::Config {
            type_: maplit::hashmap! {
                type_name => crate::config::TypeEngineConfig {
                    ..Default::default()
                },
            },
            ..Default::default()
        };
        engine.set_overrides(config);

        let cwd = std::path::Path::new(".").canonicalize().unwrap();
        let result = engine.init_dir(&cwd);
        assert!(result.is_err());
    }

    #[test]
    fn test_policy_default() {
        let storage = ConfigStorage::new();
        let mut engine = ConfigEngine::new(&storage);
        engine.set_isolated(true);

        let config = crate::config::Config::default();
        engine.set_overrides(config);

        let cwd = std::path::Path::new(".").canonicalize().unwrap();
        engine.init_dir(&cwd).unwrap();
        let policy = engine.policy(&cwd.join("Cargo.toml"));
        assert!(!policy.binary);
    }

    #[test]
    fn test_policy_fallback() {
        let storage = ConfigStorage::new();
        let mut engine = ConfigEngine::new(&storage);
        engine.set_isolated(true);

        let type_name = kstring::KString::from_static(NEVER_EXIST_TYPE);

        let config = crate::config::Config {
            default: crate::config::EngineConfig {
                binary: Some(true),
                ..Default::default()
            },
            type_: maplit::hashmap! {
                type_name.clone() => crate::config::TypeEngineConfig {
                    extend_glob: vec![type_name],
                    engine: crate::config::EngineConfig {
                        binary: Some(false),
                        ..Default::default()
                    },
                },
            },
            ..Default::default()
        };
        engine.set_overrides(config);

        let cwd = std::path::Path::new(".").canonicalize().unwrap();
        engine.init_dir(&cwd).unwrap();
        let policy = engine.policy(&cwd.join("Cargo.toml"));
        assert!(policy.binary);
    }

    #[test]
    fn test_policy_type_specific() {
        let storage = ConfigStorage::new();
        let mut engine = ConfigEngine::new(&storage);
        engine.set_isolated(true);

        let type_name = kstring::KString::from_static(NEVER_EXIST_TYPE);

        let config = crate::config::Config {
            default: crate::config::EngineConfig {
                binary: Some(true),
                ..Default::default()
            },
            type_: maplit::hashmap! {
                type_name.clone() => crate::config::TypeEngineConfig {
                    extend_glob: vec![type_name],
                    engine: crate::config::EngineConfig {
                        binary: Some(false),
                        ..Default::default()
                    },
                },
            },
            ..Default::default()
        };
        engine.set_overrides(config);

        let cwd = std::path::Path::new(".").canonicalize().unwrap();
        engine.init_dir(&cwd).unwrap();
        let policy = engine.policy(&cwd.join("Cargo.toml"));
        assert!(policy.binary);
        let policy = engine.policy(&cwd.join(NEVER_EXIST_TYPE));
        assert!(!policy.binary);
    }
}
