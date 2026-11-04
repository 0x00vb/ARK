use super::{refuse, InferenceConfig};
use crate::{Backend, BackendKind, ModelOption, Result};
use serde::Deserialize;
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

const LIMIT: u64 = 64 << 10;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Installation {
    version: u32,
    executable: PathBuf,
    models: Vec<Model>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Model {
    id: String,
    label: String,
    profile: String,
    path: PathBuf,
}

pub(crate) struct Installed {
    executable: PathBuf,
    models: Vec<Model>,
}

fn metadata_json(path: &Path) -> std::result::Result<serde_json::Value, String> {
    bounded_json(path, LIMIT)
}

fn bounded_json(path: &Path, limit: u64) -> std::result::Result<serde_json::Value, String> {
    let file =
        fs::File::open(path).map_err(|_| "Installed model metadata is missing.".to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() > limit {
        return Err("Installed model metadata exceeds its limit.".into());
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("Installed model metadata exceeds its limit.".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "Installed model metadata is invalid.".into())
}

fn executable(path: &Path) -> bool {
    if !path.is_absolute() || !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
    }
}

fn flint_model(path: &Path) -> bool {
    if !path.is_absolute() || !path.is_dir() {
        return false;
    }
    let Ok(manifest) = metadata_json(&path.join("flint-int4.json")) else {
        return false;
    };
    if manifest.get("weight_format").and_then(|v| v.as_str()) != Some("mlx-affine-int4") {
        return false;
    }
    let Some(files) = manifest.get("checkpoint_files").and_then(|v| v.as_object()) else {
        return false;
    };
    if files.is_empty() || files.len() > 256 {
        return false;
    }
    let Ok(verified) = metadata_json(&path.join("verified.json")) else {
        return false;
    };
    let Ok(config) = metadata_json(&path.join("config.json")) else {
        return false;
    };
    if !config
        .get("architectures")
        .and_then(|v| v.as_array())
        .is_some_and(|a| a.len() == 1 && a[0].as_str() == Some("FlintForCausalLM"))
    {
        return false;
    }
    let Ok(tokenizer) = bounded_json(&path.join("tokenizer.json"), 32 << 20) else {
        return false;
    };
    if tokenizer.pointer("/model/type").and_then(|v| v.as_str()) != Some("BPE")
        || !tokenizer
            .pointer("/model/vocab")
            .and_then(|v| v.as_object())
            .is_some_and(|v| !v.is_empty())
    {
        return false;
    }
    let mut total = 0u64;
    let mut shards = 0;
    for name in files.keys() {
        let file = Path::new(name);
        if file.components().count() != 1
            || !matches!(
                file.components().next(),
                Some(std::path::Component::Normal(_))
            )
        {
            return false;
        }
        let Ok(meta) = fs::metadata(path.join(file)) else {
            return false;
        };
        let Some(size) = verified
            .get(name)
            .and_then(|v| v.get("size"))
            .and_then(|v| v.as_u64())
        else {
            return false;
        };
        let Some(next) = total.checked_add(size).filter(|v| *v <= 32 << 30) else {
            return false;
        };
        total = next;
        if !meta.is_file() || size == 0 || meta.len() != size {
            return false;
        }
        if name.ends_with(".safetensors") {
            shards += 1;
        }
    }
    shards > 0 && files.contains_key("config.json") && files.contains_key("tokenizer.json")
}

fn bridge_path(
    root: &Path,
    home: Option<&Path>,
    windows: bool,
) -> std::result::Result<PathBuf, String> {
    if windows {
        Ok(root.join("bin/ark-inference-bridge-flint.exe"))
    } else {
        home.map(|h| h.join(".local/bin/ark-inference-bridge-flint"))
            .ok_or_else(|| "Cannot locate the installed ARK Inference executable.".into())
    }
}

impl Installed {
    fn at(
        root: &Path,
        legacy_bridge: &Path,
        executable_override: Option<&Path>,
    ) -> std::result::Result<Self, String> {
        let manifest = root.join("installation.json");
        let registered = match fs::symlink_metadata(&manifest) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.to_string()),
        };
        let mut installation = if registered {
            let value = metadata_json(&manifest)?;
            serde_json::from_value::<Installation>(value)
                .map_err(|_| "ARK Inference installation metadata is invalid.".to_string())?
        } else {
            Installation {
                version: 1,
                executable: legacy_bridge.into(),
                models: vec![Model {
                    id: "flint".into(),
                    label: "Flint".into(),
                    profile: "flint-v2".into(),
                    path: root.join("models/flint"),
                }],
            }
        };
        if let Some(executable) = executable_override {
            installation.executable = executable.to_path_buf();
        }
        if installation.version != 1
            || !executable(&installation.executable)
            || installation.models.is_empty()
            || installation.models.len() > 256
        {
            return Err("ARK Inference is not installed or its installation is incomplete.".into());
        }
        let mut ids = HashSet::new();
        for model in &installation.models {
            if !ids.insert(model.id.clone()) {
                return Err("Duplicate installed model identifiers.".into());
            }
        }
        let models: Vec<_> = installation
            .models
            .into_iter()
            .filter(|model| {
                !model.id.is_empty()
                    && model.id.len() <= 128
                    && model
                        .id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
                    && !model.label.trim().is_empty()
                    && model.label.len() <= 128
                    && !model.label.chars().any(char::is_control)
                    && model.profile == "flint-v2"
                    && flint_model(&model.path)
            })
            .collect();
        if models.is_empty() {
            return Err("No complete supported ARK Inference models are installed.".into());
        }
        Ok(Self {
            executable: installation.executable,
            models,
        })
    }

    fn discover(executable_override: Option<&Path>) -> std::result::Result<Self, String> {
        let root = ark_os::dirs::app_home(".local/share/ark-inference", "ARK Inference")
            .ok_or("Cannot locate the ARK Inference installation folder.")?;
        let bridge = bridge_path(&root, ark_os::dirs::home().as_deref(), cfg!(windows))?;
        Self::at(&root, &bridge, executable_override)
    }

    fn config(&self, id: &str) -> Result<InferenceConfig> {
        let model = self
            .models
            .iter()
            .find(|m| m.id == id)
            .ok_or_else(|| refuse("The selected ARK Inference model is not installed."))?;
        let mut config = InferenceConfig::new(&self.executable, &model.path);
        config.flint_tools = true;
        config.context = 12288;
        config.output_tokens = 4096;
        config.array_bytes = 3 << 30;
        config.file_bytes = 32 << 30;
        config.threads = 8;
        config.backend = if cfg!(target_os = "macos") {
            "metal"
        } else {
            "cpu"
        }
        .into();
        config.prefill_timeout = Duration::from_secs(1200);
        config.call_timeout = Duration::from_secs(1800);
        config.check()?;
        Ok(config)
    }
}

pub(crate) fn resolve(id: &str, executable_override: Option<&Path>) -> Result<InferenceConfig> {
    Installed::discover(executable_override)
        .map_err(|e| refuse(&e))?
        .config(id)
}

pub(crate) fn backend() -> Backend {
    let executable_override = std::env::var_os("ARK_INFERENCE_EXECUTABLE").map(PathBuf::from);
    match Installed::discover(executable_override.as_deref()) {
        Ok(installed) => Backend {
            provider: "inference".into(),
            label: "ARK Inference".into(),
            kind: BackendKind::Local,
            ready: true,
            detail: "Installed on this computer".into(),
            fix: None,
            models: installed.models.iter().map(|m| m.id.clone()).collect(),
            model_options: installed
                .models
                .iter()
                .map(|m| ModelOption {
                    id: m.id.clone(),
                    label: m.label.clone(),
                    efforts: vec![],
                    default_effort: None,
                })
                .collect(),
            step: None,
        },
        Err(detail) => Backend {
            provider: "inference".into(),
            label: "ARK Inference".into(),
            kind: BackendKind::Local,
            ready: false,
            detail,
            fix: None,
            models: vec![],
            model_options: vec![],
            step: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Fixture {
        root: PathBuf,
        bridge: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("ark-installed-{}", uuid::Uuid::new_v4()));
            let model = root.join("models/flint");
            fs::create_dir_all(&model).unwrap();
            let bridge = root.join("bridge.exe");
            fs::write(&bridge, b"filesystem fixture, never executed").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&bridge, fs::Permissions::from_mode(0o700)).unwrap();
            }
            let mut files = serde_json::Map::new();
            let mut verified = serde_json::Map::new();
            for name in ["config.json", "tokenizer.json", "model-00000.safetensors"] {
                let bytes: &[u8] = match name {
                    "config.json" => br#"{"architectures":["FlintForCausalLM"]}"#,
                    "tokenizer.json" => br#"{"model":{"type":"BPE","vocab":{"fixture":0}}}"#,
                    _ => b"fixture",
                };
                fs::write(model.join(name), bytes).unwrap();
                files.insert(name.into(), json!("not a weight integrity test"));
                verified.insert(name.into(), json!({"size":bytes.len()}));
            }
            fs::write(
                model.join("flint-int4.json"),
                json!({"weight_format":"mlx-affine-int4","checkpoint_files":files}).to_string(),
            )
            .unwrap();
            fs::write(
                model.join("verified.json"),
                serde_json::Value::Object(verified).to_string(),
            )
            .unwrap();
            Self { root, bridge }
        }
        fn manifest(&self, models: serde_json::Value) {
            fs::write(
                self.root.join("installation.json"),
                json!({"version":1,"executable":self.bridge,"models":models}).to_string(),
            )
            .unwrap();
        }
        fn model(&self) -> serde_json::Value {
            json!({"id":"flint","label":"Flint","profile":"flint-v2","path":self.root.join("models/flint")})
        }
        fn load(&self) -> std::result::Result<Installed, String> {
            Installed::at(&self.root, &self.bridge, None)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[test]
    fn installed_flint_uses_one_named_model_and_proven_limits() {
        let f = Fixture::new();
        let legacy = f.load().unwrap();
        let c = legacy.config("flint").unwrap();
        assert!(c.flint_tools);
        assert_eq!(c.executable, f.bridge);
        assert_eq!(c.model, f.root.join("models/flint"));
        assert_eq!(
            (c.context, c.output_tokens, c.file_bytes),
            (12288, 4096, 32 << 30)
        );
        assert!(legacy.config("unknown").is_err());
        f.manifest(json!([f.model()]));
        assert_eq!(f.load().unwrap().config("flint").unwrap().model, c.model);
    }

    #[test]
    fn incomplete_models_and_missing_executables_are_not_ready() {
        let f = Fixture::new();
        fs::write(
            f.root.join("models/flint/model-00000.safetensors"),
            b"short",
        )
        .unwrap();
        assert!(f.load().is_err());
        fs::write(
            f.root.join("models/flint/model-00000.safetensors"),
            b"fixture",
        )
        .unwrap();
        fs::remove_file(f.root.join("models/flint/tokenizer.json")).unwrap();
        assert!(f.load().is_err());
        fs::write(
            f.root.join("models/flint/tokenizer.json"),
            br#"{"model":{"type":"BPE","vocab":{"fixture":0}}}"#,
        )
        .unwrap();
        fs::remove_file(&f.bridge).unwrap();
        assert!(f.load().is_err());
    }

    #[test]
    fn invalid_registry_never_falls_back_to_legacy() {
        let f = Fixture::new();
        f.manifest(json!([f.model(), f.model()]));
        assert!(f.load().is_err());
        let mut model = f.model();
        model["profile"] = json!("unknown");
        f.manifest(json!([model]));
        assert!(f.load().is_err());
        fs::write(f.root.join("installation.json"), b"{").unwrap();
        assert!(f.load().is_err());
        fs::write(
            f.root.join("installation.json"),
            vec![b' '; (LIMIT + 1) as usize],
        )
        .unwrap();
        assert!(f.load().is_err());
    }

    #[test]
    fn explicit_configuration_keeps_override_precedence() {
        let f = Fixture::new();
        let custom = f.root.join("custom-bridge.exe");
        fs::copy(&f.bridge, &custom).unwrap();
        let absolute = f.root.join("custom-model.gguf");
        let c = super::super::installed_config(
            Some(custom.clone()),
            absolute.to_str().unwrap(),
            |_, _| panic!("explicit paths must not discover models"),
        )
        .unwrap();
        assert_eq!(c.executable, custom);
        assert_eq!(c.model, absolute);
        assert!(!c.flint_tools);
        let c = super::super::installed_config(Some(custom.clone()), "flint", |id, exe| {
            Installed::at(&f.root, &f.bridge, exe).unwrap().config(id)
        })
        .unwrap();
        assert_eq!(c.executable, custom);
        assert!(c.flint_tools);
        assert_eq!(c.file_bytes, 32 << 30);
        assert!(
            super::super::installed_config(None, absolute.to_str().unwrap(), |_, _| panic!(
                "absolute models require explicit executable"
            ))
            .is_err()
        );
    }

    #[test]
    fn custom_executable_works_without_default_bridge() {
        let f = Fixture::new();
        let custom = f.root.join("custom.exe");
        fs::copy(&f.bridge, &custom).unwrap();
        fs::remove_file(&f.bridge).unwrap();
        assert!(f.load().is_err());
        let installed = Installed::at(&f.root, &f.bridge, Some(&custom)).unwrap();
        assert_eq!(installed.config("flint").unwrap().executable, custom);
    }

    #[test]
    fn mixed_registry_retains_only_complete_supported_models() {
        let f = Fixture::new();
        let mut unsupported = f.model();
        unsupported["id"] = json!("future");
        unsupported["profile"] = json!("future-profile");
        let mut missing = f.model();
        missing["id"] = json!("missing");
        missing["path"] = json!(f.root.join("absent"));
        f.manifest(json!([f.model(), unsupported, missing]));
        let installed = f.load().unwrap();
        assert_eq!(installed.models.len(), 1);
        assert_eq!(installed.models[0].id, "flint");
    }

    #[test]
    fn malformed_supported_configuration_is_not_ready() {
        let f = Fixture::new();
        let path = f.root.join("models/flint/config.json");
        let size = fs::metadata(&path).unwrap().len();
        fs::write(path, vec![b'!'; size as usize]).unwrap();
        assert!(f.load().is_err());
        let f = Fixture::new();
        let path = f.root.join("models/flint/tokenizer.json");
        let size = fs::metadata(&path).unwrap().len();
        fs::write(path, vec![b'!'; size as usize]).unwrap();
        assert!(f.load().is_err());
    }

    #[test]
    fn total_checkpoint_bytes_respect_the_installed_file_allowance() {
        let f = Fixture::new();
        let model = f.root.join("models/flint");
        let shard = model.join("model-00000.safetensors");
        let file = fs::OpenOptions::new().write(true).open(shard).unwrap();
        file.set_len(32u64 << 30).unwrap();
        let mut verified: serde_json::Value = metadata_json(&model.join("verified.json")).unwrap();
        verified["model-00000.safetensors"]["size"] = json!(32u64 << 30);
        fs::write(model.join("verified.json"), verified.to_string()).unwrap();
        assert!(
            f.load().is_err(),
            "metadata plus checkpoint exceeds the32GiB file allowance"
        );
    }

    #[test]
    fn installation_layout_is_injected_without_environment_changes() {
        let f = Fixture::new();
        assert_eq!(
            bridge_path(&f.root, None, true).unwrap(),
            f.root.join("bin/ark-inference-bridge-flint.exe")
        );
        assert_eq!(
            bridge_path(&f.root, Some(&f.root), false).unwrap(),
            f.root.join(".local/bin/ark-inference-bridge-flint")
        );
        assert!(bridge_path(&f.root, None, false).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn non_executable_bridge_is_not_ready() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new();
        fs::set_permissions(&f.bridge, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(f.load().is_err());
    }
}
