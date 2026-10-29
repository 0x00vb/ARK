//! Person-installed, data-only skill packages supplied by the application.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use ark_core::{ArtifactId, ProjectId};
use serde::{Deserialize, Serialize};

use crate::{AgentError, Engine};

const RECORD: &str = "skill_package";
const MAX_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackagedSkill {
    pub name: String,
    pub files: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SkillPackage {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub publisher: String,
    pub version: String,
    pub skills: Vec<PackagedSkill>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Receipt {
    schema_version: u16,
    package: SkillPackage,
    digest: String,
    active: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct PackageView {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub publisher: String,
    pub version: String,
    pub digest: String,
    pub skills: Vec<String>,
    pub installed: bool,
    pub installed_version: Option<String>,
    pub update_available: bool,
    pub available: bool,
    pub error: Option<String>,
}

fn invalid(message: impl Into<String>) -> AgentError {
    AgentError::Plugin(message.into())
}

fn name(value: &str) -> bool {
    super::valid_name(value)
        && !value.starts_with('-')
        && !value.ends_with('-')
        && !value.contains("--")
}

fn relative(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.split('/').all(|part| {
            !part.is_empty() && part != "." && part != ".." && !part.ends_with(['.', ' '])
        })
        && !value.contains(['\\', ':'])
        && Path::new(value).components().count() <= 8
        && Path::new(value).components().all(
            |part| matches!(part, Component::Normal(s) if !s.to_string_lossy().starts_with('.')),
        )
        && matches!(
            Path::new(value).extension().and_then(|s| s.to_str()),
            Some("md" | "txt" | "json" | "csv")
        )
}

impl SkillPackage {
    pub fn validate(&self) -> Result<(), AgentError> {
        if self.id.is_empty()
            || self.id.len() > 128
            || !self.id.split('.').all(name)
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        {
            return Err(invalid("Invalid skill package id"));
        }
        if self.name.is_empty()
            || self.name.len() > 256
            || self.summary.len() > 1024
            || self.publisher.is_empty()
            || self.publisher.len() > 256
            || self.version.is_empty()
            || self.version.len() > 128
            || self.skills.is_empty()
            || self.skills.len() > 32
        {
            return Err(invalid("Invalid skill package metadata"));
        }
        let mut names = BTreeSet::new();
        let mut total = 0;
        let mut count = 0;
        for skill in &self.skills {
            if !name(&skill.name) || !names.insert(&skill.name) {
                return Err(invalid("Invalid or duplicate packaged skill name"));
            }
            let text = skill
                .files
                .get("SKILL.md")
                .ok_or_else(|| invalid("A packaged skill needs SKILL.md"))?;
            if text.len() > 65536 {
                return Err(invalid("SKILL.md is too large"));
            }
            let (front, _) = super::split_front_matter(text);
            if front.get("name") != Some(&skill.name)
                || front
                    .get("description")
                    .is_none_or(|s| s.trim().is_empty() || s.len() > 1024)
            {
                return Err(invalid("Invalid SKILL.md name or description"));
            }
            for path in skill.files.keys() {
                if skill
                    .files
                    .keys()
                    .any(|other| other.starts_with(&format!("{path}/")))
                {
                    return Err(invalid("A skill package file cannot also be a directory"));
                }
            }
            for (path, content) in &skill.files {
                total += content.len();
                count += 1;
                if !relative(path)
                    || content.len() > 65536
                    || content
                        .chars()
                        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
                    || total > MAX_BYTES
                    || count > 64
                {
                    return Err(invalid("Skill package path, file or size is unsupported"));
                }
            }
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<String, AgentError> {
        self.validate()?;
        Ok(ArtifactId::of_bytes(&serde_json::to_vec(self)?).0)
    }
}

fn directory(path: &Path) -> Result<(), AgentError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(invalid("Skill package directory is redirected or invalid"));
    }
    Ok(())
}

fn child_directory(root: &Path, child: &str) -> Result<PathBuf, AgentError> {
    directory(root)?;
    let path = root.join(child);
    match fs::create_dir(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    directory(&path)?;
    Ok(path)
}

fn exists(path: &Path) -> Result<bool, AgentError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn base(engine: &Engine) -> Result<PathBuf, AgentError> {
    child_directory(engine.store.root(), "skill-packages")
}

fn lock(root: &Path) -> Result<File, AgentError> {
    let path = root.join("operation.lock");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000);
    }
    let file = options.open(&path)?;
    if fs::symlink_metadata(&path)?.file_type().is_symlink() || !file.metadata()?.is_file() {
        return Err(invalid("Invalid skill package operation lock"));
    }
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(invalid("Another skill package operation is running")),
        Err(TryLockError::Error(e)) => Err(e.into()),
    }
}

fn receipts(engine: &Engine) -> Result<Vec<Receipt>, AgentError> {
    let values = engine.store.list_records(RECORD, Some("store"))?;
    if values.len() > 128 {
        return Err(invalid("Too many installed skill packages"));
    }
    values
        .into_iter()
        .map(|value| {
            let receipt: Receipt = serde_json::from_value(value)?;
            if receipt.schema_version != 1 || receipt.package.digest()? != receipt.digest {
                return Err(invalid(
                    "Invalid skill package receipt; existing data was preserved",
                ));
            }
            Ok(receipt)
        })
        .collect()
}

fn release(engine: &Engine, digest: &str) -> Result<PathBuf, AgentError> {
    let root = engine.store.root().join("skill-packages");
    directory(&root)?;
    let path = root.join(digest);
    directory(&path)?;
    Ok(path)
}

fn verify(package: &SkillPackage, root: &Path) -> Result<(), AgentError> {
    directory(root)?;
    let expected: BTreeMap<PathBuf, &str> = package
        .skills
        .iter()
        .flat_map(|skill| {
            skill
                .files
                .iter()
                .map(move |(path, content)| (Path::new(&skill.name).join(path), content.as_str()))
        })
        .collect();
    let mut allowed_dirs = BTreeSet::new();
    for path in expected.keys() {
        let mut parent = path.parent();
        while let Some(dir) = parent.filter(|dir| !dir.as_os_str().is_empty()) {
            allowed_dirs.insert(dir.to_path_buf());
            parent = dir.parent();
        }
    }
    let mut pending = vec![root.to_owned()];
    let mut found = BTreeSet::new();
    let mut entries = 0;
    while let Some(dir) = pending.pop() {
        directory(&dir)?;
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            let metadata = fs::symlink_metadata(&path)?;
            entries += 1;
            if entries > 1024 || metadata.file_type().is_symlink() {
                return Err(invalid(
                    "Skill package files changed; local data was preserved",
                ));
            }
            if metadata.is_dir() {
                let relative = path
                    .strip_prefix(root)
                    .map_err(|e| invalid(e.to_string()))?;
                if !allowed_dirs.contains(relative) {
                    return Err(invalid(
                        "Skill package has unowned directories; local data was preserved",
                    ));
                }
                pending.push(path);
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|e| invalid(e.to_string()))?
                .to_path_buf();
            let content = expected.get(&relative).ok_or_else(|| {
                invalid("Skill package has unowned files; local data was preserved")
            })?;
            if !metadata.is_file()
                || metadata.len() > 65536
                || fs::read(&path)? != content.as_bytes()
            {
                return Err(invalid(
                    "Skill package files changed; local data was preserved",
                ));
            }
            found.insert(relative);
        }
    }
    if found.len() != expected.len() {
        return Err(invalid(
            "Skill package is incomplete; existing data was preserved",
        ));
    }
    Ok(())
}

pub fn list(engine: &Engine) -> Result<Vec<PackageView>, AgentError> {
    let installed = receipts(engine)?;
    let mut packages: Vec<(&SkillPackage, bool)> = engine
        .skill_packages()
        .iter()
        .map(|p| (p.as_ref(), true))
        .collect();
    for receipt in &installed {
        if receipt.active && !packages.iter().any(|(p, _)| p.id == receipt.package.id) {
            packages.push((&receipt.package, false));
        }
    }
    packages
        .into_iter()
        .map(|(package, available)| {
            let digest = package.digest()?;
            let receipt = installed
                .iter()
                .find(|r| r.package.id == package.id && r.active);
            let error = receipt
                .and_then(|r| {
                    release(engine, &r.digest)
                        .and_then(|root| verify(&r.package, &root))
                        .err()
                })
                .map(|e| e.to_string());
            Ok(PackageView {
                id: package.id.clone(),
                name: package.name.clone(),
                summary: package.summary.clone(),
                publisher: package.publisher.clone(),
                version: package.version.clone(),
                digest: digest.clone(),
                skills: package.skills.iter().map(|s| s.name.clone()).collect(),
                installed: receipt.is_some(),
                installed_version: receipt.map(|r| r.package.version.clone()),
                update_available: receipt.is_some_and(|r| r.digest != digest),
                available,
                error,
            })
        })
        .collect()
}

pub fn directories(engine: &Engine) -> Result<Vec<(String, PathBuf)>, AgentError> {
    let mut out = Vec::new();
    for receipt in receipts(engine)?.into_iter().filter(|r| r.active) {
        match release(engine, &receipt.digest).and_then(|root| {
            verify(&receipt.package, &root)?;
            Ok(root)
        }) {
            Ok(root) => out.push((receipt.package.id, root)),
            Err(error) => {
                tracing::warn!(package = %receipt.package.id, %error, "Installed skill package needs attention")
            }
        }
    }
    Ok(out)
}

fn prepare(package: &SkillPackage, root: &Path) -> Result<String, AgentError> {
    let digest = package.digest()?;
    let target = root.join(&digest);
    match fs::symlink_metadata(&target) {
        Ok(_) => {
            verify(package, &target)?;
            return Ok(digest);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let stage = root.join(format!(".stage-{}", ProjectId::new().as_str()));
    fs::create_dir(&stage)?;
    let mut dirs = BTreeSet::from([stage.clone()]);
    for skill in &package.skills {
        let skill_root = child_directory(&stage, &skill.name)?;
        dirs.insert(skill_root.clone());
        for (relative, content) in &skill.files {
            let path = Path::new(relative);
            let mut parent = skill_root.clone();
            if let Some(parts) = path.parent() {
                for component in parts.components() {
                    let Component::Normal(part) = component else {
                        return Err(invalid("Invalid package path"));
                    };
                    parent = child_directory(&parent, &part.to_string_lossy())?;
                    dirs.insert(parent.clone());
                }
            }
            let mut file = OpenOptions::new().write(true).create_new(true).open(
                parent.join(
                    path.file_name()
                        .ok_or_else(|| invalid("Missing package filename"))?,
                ),
            )?;
            file.write_all(content.as_bytes())?;
            file.sync_all()?;
        }
    }
    verify(package, &stage)?;
    #[cfg(unix)]
    for dir in dirs.iter().rev() {
        File::open(dir)?.sync_all()?;
    }
    fs::rename(&stage, &target)?;
    #[cfg(unix)]
    File::open(root)?.sync_all()?;
    Ok(digest)
}

pub fn preview(source: &str) -> Result<SkillPackage, AgentError> {
    if source.len() > MAX_BYTES {
        return Err(invalid("Skill package exceeds the 1 MiB import limit"));
    }
    let package: SkillPackage = serde_json::from_str(source)?;
    package.validate()?;
    Ok(package)
}

pub fn export(engine: &Engine, id: &str) -> Result<SkillPackage, AgentError> {
    let receipt = receipts(engine)?
        .into_iter()
        .find(|r| r.package.id == id)
        .ok_or_else(|| invalid("That skill package is not installed"))?;
    verify(&receipt.package, &release(engine, &receipt.digest)?)?;
    Ok(receipt.package)
}

pub fn import(
    engine: &Engine,
    project: &ProjectId,
    source: &str,
    digest: &str,
) -> Result<PackageView, AgentError> {
    let package = preview(source)?;
    if engine
        .skill_packages()
        .iter()
        .any(|p| p.id == package.id && p.digest().ok().as_deref() != Some(digest))
    {
        return Err(invalid("An application package already owns this id"));
    }
    install_data(engine, project, &package, digest)
}

pub fn install(
    engine: &Engine,
    project: &ProjectId,
    id: &str,
    expected_digest: &str,
) -> Result<PackageView, AgentError> {
    let package = engine
        .skill_packages()
        .iter()
        .find(|p| p.id == id)
        .ok_or_else(|| invalid("No reviewed skill package with that id"))?;
    install_data(engine, project, package, expected_digest)
}

fn install_data(
    engine: &Engine,
    project: &ProjectId,
    package: &SkillPackage,
    expected_digest: &str,
) -> Result<PackageView, AgentError> {
    let id = package.id.as_str();
    if package.digest()? != expected_digest {
        return Err(invalid("Skill package changed; review the current version"));
    }
    let root = base(engine)?;
    let _lock = lock(&root)?;
    let existing = receipts(engine)?;
    if existing.len() == 128 && !existing.iter().any(|r| r.package.id == id) {
        return Err(invalid(
            "Skill package receipt limit reached; nothing was installed",
        ));
    }
    let workspace = engine.workspace_dir(project)?;
    let local = super::discover(&[
        (workspace.join(".agents/skills"), super::SkillScope::Project),
        (engine.store.root().join("skills"), super::SkillScope::Store),
    ]);
    let builtin = super::discover(
        &engine
            .skill_dirs()
            .iter()
            .map(|p| (p.clone(), super::SkillScope::Builtin))
            .collect::<Vec<_>>(),
    );
    for skill in &package.skills {
        if exists(&workspace.join(".agents/skills").join(&skill.name))?
            || exists(&engine.store.root().join("skills").join(&skill.name))?
            || local.iter().chain(&builtin).any(|s| s.name == skill.name)
            || existing.iter().any(|r| {
                r.active
                    && r.package.id != id
                    && r.package.skills.iter().any(|s| s.name == skill.name)
            })
        {
            return Err(invalid(format!(
                "A skill named {} already exists; nothing was replaced",
                skill.name
            )));
        }
    }
    let digest = prepare(package, &root)?;
    let receipt = Receipt {
        schema_version: 1,
        package: package.clone(),
        digest,
        active: true,
    };
    engine
        .store
        .put_record(RECORD, id, "store", &serde_json::to_value(receipt)?)?;
    list(engine)?
        .into_iter()
        .find(|row| row.id == id)
        .ok_or_else(|| invalid("Installed package could not be read"))
}

pub fn remove(engine: &Engine, id: &str) -> Result<(), AgentError> {
    let root = base(engine)?;
    let _lock = lock(&root)?;
    let mut receipt = receipts(engine)?
        .into_iter()
        .find(|r| r.package.id == id)
        .ok_or_else(|| invalid("That skill package is not installed"))?;
    receipt.active = false;
    engine
        .store
        .put_record(RECORD, id, "store", &serde_json::to_value(receipt)?)?;
    Ok(())
}
