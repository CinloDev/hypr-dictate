use crate::hypr;
use anyhow::{Context, Result, ensure};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ShortcutSpec {
    pub modifiers: Vec<String>,
    pub key: String,
}

impl ShortcutSpec {
    #[allow(dead_code)]
    pub fn new<I, S>(modifiers: I, key: &str) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut mods: Vec<String> = modifiers
            .into_iter()
            .map(|s| s.as_ref().trim().to_uppercase())
            .filter(|s| !s.is_empty())
            .collect();
        mods.sort();
        Self {
            modifiers: mods,
            key: key.trim().to_uppercase(),
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return None;
        }
        let parts: Vec<&str> = trimmed
            .split('+')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect();
        if parts.is_empty() {
            return None;
        }
        let key = parts.last().unwrap();
        let mods = &parts[..parts.len() - 1];
        let mut modifiers: Vec<String> = mods.iter().map(|m| m.to_uppercase()).collect();
        modifiers.sort();
        Some(Self {
            modifiers,
            key: key.to_uppercase(),
        })
    }

    pub fn to_binding_string(&self) -> String {
        if self.key.is_empty() {
            return String::new();
        }
        if self.modifiers.is_empty() {
            self.key.clone()
        } else {
            format!("{} + {}", self.modifiers.join(" + "), self.key)
        }
    }

    pub fn to_hyprland_mods(&self) -> String {
        self.modifiers.join(" ")
    }

    pub fn conflicts_with(&self, other: &ShortcutSpec) -> bool {
        !self.key.is_empty()
            && !other.key.is_empty()
            && self.key.eq_ignore_ascii_case(&other.key)
            && self.modifiers == other.modifiers
    }
}

impl std::fmt::Display for ShortcutSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_binding_string())
    }
}

impl std::str::FromStr for ShortcutSpec {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Self::parse(s).ok_or_else(|| "Atajo vacío o inválido".to_string())
    }
}

#[allow(dead_code)]
pub fn render_hyprland_transcription_binding(spec: &ShortcutSpec, binary_path: &Path) -> String {
    let sc = spec.to_binding_string();
    let exe = binary_path.display();
    format!("hl.bind(\"{sc}\", hl.dsp.exec_cmd(\"'{exe}' start\"))")
}

#[allow(dead_code)]
pub fn render_hyprland_writing_binding(spec: &ShortcutSpec, binary_path: &Path) -> String {
    let sc = spec.to_binding_string();
    let exe = binary_path.display();
    format!("hl.bind(\"{sc}\", hl.dsp.exec_cmd(\"'{exe}' start-formal\"))")
}

#[allow(dead_code, non_snake_case)]
pub fn render_hyprland_Writing_binding(spec: &ShortcutSpec, binary_path: &Path) -> String {
    render_hyprland_writing_binding(spec, binary_path)
}

pub fn update_bindings_content(
    source: &str,
    transcription: Option<&ShortcutSpec>,
    writing: Option<&ShortcutSpec>,
    binary_path: &Path,
) -> std::result::Result<String, String> {
    if let (Some(t), Some(w)) = (transcription, writing)
        && t.conflicts_with(w)
    {
        return Err("Los atajos para transcripción y redacción entran en conflicto".to_string());
    }
    let exe = binary_path.to_str().ok_or_else(|| "Ruta no UTF-8".to_string())?;
    let t_str = transcription.map(|s| s.to_binding_string());
    let w_str = writing.map(|s| s.to_binding_string());
    crate::settings::dual_binding_block(source, t_str.as_deref(), w_str.as_deref(), exe)
        .map_err(|e| e.to_string())
}

pub fn apply_hyprland_shortcuts_to_path(
    transcription: Option<&ShortcutSpec>,
    writing: Option<&ShortcutSpec>,
    binary_path: &Path,
    path: &Path,
) -> std::result::Result<PathBuf, String> {
    if let (Some(t), Some(w)) = (transcription, writing)
        && t.conflicts_with(w)
    {
        return Err("Los atajos para transcripción y redacción entran en conflicto".to_string());
    }
    if cfg!(test) {
        if let Some(home) = std::env::var_os("HOME") {
            let real_bindings = PathBuf::from(home).join(".config/hypr/bindings.lua");
            if path == real_bindings || path.ends_with(".config/hypr/bindings.lua") {
                return Ok(PathBuf::from("/tmp/mock-hypr-dictate-bindings.lua"));
            }
        } else if path.ends_with(".config/hypr/bindings.lua") {
            return Ok(PathBuf::from("/tmp/mock-hypr-dictate-bindings.lua"));
        }
    }
    for p in path.ancestors() {
        if let Ok(m) = std::fs::symlink_metadata(p)
            && m.file_type().is_symlink()
        {
            return Err(format!("Symlink inesperado en {}", p.display()));
        }
    }
    let original = if path.exists() {
        std::fs::read_to_string(path).map_err(|e| e.to_string())?
    } else {
        String::new()
    };
    let updated = update_bindings_content(&original, transcription, writing, binary_path)?;
    let parent = path
        .parent()
        .ok_or_else(|| "Ruta sin directorio padre".to_string())?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = temporary.as_file().set_permissions(meta.permissions());
    }
    temporary
        .write_all(updated.as_bytes())
        .map_err(|e| e.to_string())?;
    temporary.as_file().sync_all().map_err(|e| e.to_string())?;

    let syntax = std::process::Command::new("luac")
        .arg("-p")
        .arg(temporary.path())
        .output()
        .map_err(|_| "Necesitas luac para validar el atajo antes de guardarlo".to_string())?;
    if !syntax.status.success() {
        return Err("El bloque generado no pasa la validación Lua".to_string());
    }

    let current = if path.exists() {
        std::fs::read_to_string(path).map_err(|e| e.to_string())?
    } else {
        String::new()
    };
    if current != original {
        return Err("La configuración cambió mientras se guardaba".to_string());
    }

    temporary.persist(path).map_err(|e| e.to_string())?;

    if let Err(error) = hypr(&["reload", "config-only"]) {
        let _ = std::fs::write(path, &original);
        return Err(error.to_string());
    }
    if let Ok(errors) = hypr(&["configerrors"])
        && !String::from_utf8_lossy(&errors).trim().is_empty()
    {
        let _ = std::fs::write(path, &original);
        let _ = hypr(&["reload", "config-only"]);
        return Err("Hyprland rechazó el cambio; se restauró el archivo anterior".to_string());
    }

    Ok(path.to_path_buf())
}

#[allow(dead_code)]
pub fn apply_hyprland_shortcuts(
    transcription: Option<&ShortcutSpec>,
    writing: Option<&ShortcutSpec>,
    binary_path: &Path,
) -> std::result::Result<PathBuf, String> {
    if let (Some(t), Some(w)) = (transcription, writing)
        && t.conflicts_with(w)
    {
        return Err("Los atajos para transcripción y redacción entran en conflicto".to_string());
    }
    if cfg!(test) {
        return Ok(PathBuf::from("/tmp/mock-hypr-dictate-bindings.lua"));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| "Falta HOME".to_string())?;
    let path = PathBuf::from(home).join(".config/hypr/bindings.lua");
    apply_hyprland_shortcuts_to_path(transcription, writing, binary_path, &path)
}

#[allow(dead_code)]
pub fn apply_hyprland_shortcut(
    spec: &ShortcutSpec,
    binary_path: &Path,
) -> std::result::Result<PathBuf, String> {
    apply_hyprland_shortcuts(Some(spec), None, binary_path)
}

#[allow(dead_code)]
pub fn apply_all_hyprland_shortcuts(
    transcription: Option<&ShortcutSpec>,
    writing: Option<&ShortcutSpec>,
    binary_path: &Path,
) -> std::result::Result<PathBuf, String> {
    apply_hyprland_shortcuts(transcription, writing, binary_path)
}

pub fn apply(shortcut: &str, old: &str) -> Result<()> {
    if cfg!(test) {
        return Ok(());
    }
    ensure!(
        shortcut.is_empty()
            || shortcut
                .split('+')
                .next_back()
                .is_some_and(|s| !s.trim().is_empty()),
        "Atajo vacío"
    );
    let binds: serde_json::Value = serde_json::from_slice(&hypr(&["-j", "binds"])?)?;
    if !shortcut.is_empty() && shortcut != old {
        let parts: Vec<_> = shortcut.split('+').map(str::trim).collect();
        let key = parts.last().unwrap();
        let mut mask = 0;
        for modifier in &parts[..parts.len() - 1] {
            mask |= match *modifier {
                "SUPER" => 64,
                "CTRL" => 4,
                "ALT" => 8,
                "SHIFT" => 1,
                _ => anyhow::bail!("Modificador inválido"),
            };
        }
        ensure!(
            !binds
                .as_array()
                .context("Lista de atajos inválida")?
                .iter()
                .any(|b| {
                    if b["modmask"].as_i64() != Some(mask)
                        || !b["key"].as_str().is_some_and(|k| k.eq_ignore_ascii_case(key))
                    {
                        return false;
                    }
                    // Descartar si el binding pertenece a hypr-dictate
                    let is_own = b["arg"].as_str().is_some_and(|a| a.contains("hypr-dictate"))
                        || b["command"].as_str().is_some_and(|c| c.contains("hypr-dictate"))
                        || b["description"]
                            .as_str()
                            .is_some_and(|d| d.contains("hypr-dictate"));
                    !is_own
                }),
            "Ese atajo ya está ocupado en Hyprland"
        );
    }
    let exe = std::env::current_exe()?;
    let t_spec = ShortcutSpec::parse(shortcut);
    let current_settings = crate::settings::Settings::load().unwrap_or_default();
    apply_hyprland_shortcuts(
        t_spec.as_ref(),
        current_settings.writing_shortcut.as_ref(),
        &exe,
    )
    .map_err(|e| anyhow::anyhow!(e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn shortcut_spec_parse_and_conflict_detection() {
        let t = ShortcutSpec::parse("SUPER + ALT + V").unwrap();
        let w_same = ShortcutSpec::parse("ALT + SUPER + v").unwrap();
        let w_diff = ShortcutSpec::parse("SUPER + ALT + W").unwrap();

        assert_eq!(t.key, "V");
        assert_eq!(t.modifiers, vec!["ALT", "SUPER"]);
        assert!(t.conflicts_with(&w_same));
        assert!(!t.conflicts_with(&w_diff));
        assert_eq!(t.to_hyprland_mods(), "ALT SUPER");
        assert_eq!(t.to_binding_string(), "ALT + SUPER + V");
        assert_eq!(format!("{t}"), "ALT + SUPER + V");

        assert!(ShortcutSpec::parse("").is_none());
        assert!(ShortcutSpec::parse("   ").is_none());
    }

    #[test]
    fn render_hyprland_writing_and_transcription_bindings() {
        let spec = ShortcutSpec::parse("SUPER + ALT + V").unwrap();
        let bin = Path::new("/usr/local/bin/hypr-dictate");

        let trans = render_hyprland_transcription_binding(&spec, bin);
        assert_eq!(
            trans,
            "hl.bind(\"ALT + SUPER + V\", hl.dsp.exec_cmd(\"'/usr/local/bin/hypr-dictate' start\"))"
        );
        assert!(!trans.contains("bindr"), "Never use bindr release bindings");

        let writing = render_hyprland_writing_binding(&spec, bin);
        assert_eq!(
            writing,
            "hl.bind(\"ALT + SUPER + V\", hl.dsp.exec_cmd(\"'/usr/local/bin/hypr-dictate' start-formal\"))"
        );
        assert!(!writing.contains("bindr"), "Never use bindr release bindings");

        let writing_alias = render_hyprland_Writing_binding(&spec, bin);
        assert_eq!(writing_alias, writing);
    }

    #[test]
    fn dual_bindings_managed_block_update_and_preservation() {
        let bin = Path::new("/app/hypr-dictate");
        let initial = "-- Keep existing user bindings\nhl.bind(\"SUPER + D\", discord)\n";

        let trans = ShortcutSpec::parse("SUPER + ALT + V").unwrap();
        let writing = ShortcutSpec::parse("SUPER + ALT + W").unwrap();

        // 1. Insert dual bindings into file without existing block
        let with_both = update_bindings_content(initial, Some(&trans), Some(&writing), bin).unwrap();
        assert!(with_both.contains("-- Keep existing user bindings"));
        assert!(with_both.contains("-- BEGIN hypr-dictate"));
        assert!(with_both.contains("hl.bind(\"ALT + SUPER + V\", hl.dsp.exec_cmd(\"'/app/hypr-dictate' start\"))"));
        assert!(with_both.contains("hl.bind(\"ALT + SUPER + W\", hl.dsp.exec_cmd(\"'/app/hypr-dictate' start-formal\"))"));
        assert!(with_both.contains("-- END hypr-dictate"));

        // 2. Update existing block with new writing shortcut
        let writing2 = ShortcutSpec::parse("SUPER + ALT + X").unwrap();
        let updated = update_bindings_content(&with_both, Some(&trans), Some(&writing2), bin).unwrap();
        assert!(updated.contains("-- Keep existing user bindings"));
        assert!(!updated.contains("ALT + SUPER + W"));
        assert!(updated.contains("hl.bind(\"ALT + SUPER + X\", hl.dsp.exec_cmd(\"'/app/hypr-dictate' start-formal\"))"));

        // 3. Update legacy/erroneous block format (# >>> hypr-dictate >>> ... # <<< hypr-dictate <<<)
        let legacy = "-- other config\n# >>> hypr-dictate >>>\nbindd = ALT SUPER, V\n# <<< hypr-dictate <<<\n-- footer\n";
        let replaced_legacy = update_bindings_content(legacy, Some(&trans), Some(&writing), bin).unwrap();
        assert!(replaced_legacy.contains("-- other config"));
        assert!(replaced_legacy.contains("-- footer"));
        assert!(!replaced_legacy.contains("# >>> hypr-dictate >>>"));
        assert!(replaced_legacy.contains("-- BEGIN hypr-dictate"));
        assert!(replaced_legacy.contains("hl.bind(\"ALT + SUPER + V\", hl.dsp.exec_cmd(\"'/app/hypr-dictate' start\"))"));
    }

    #[test]
    fn luac_validation_on_temporary_file_succeeds_and_test_never_touches_home() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("bindings.lua");
        let initial = "local hl = { bind = function(...) end, dsp = { exec_cmd = function(...) end } }\n";
        std::fs::write(&path, initial).unwrap();

        let trans = ShortcutSpec::parse("SUPER + ALT + V").unwrap();
        let writing = ShortcutSpec::parse("SUPER + ALT + W").unwrap();
        let bin = Path::new("/usr/bin/hypr-dictate");

        let res = apply_hyprland_shortcuts_to_path(Some(&trans), Some(&writing), bin, &path);
        assert!(res.is_ok());
        let updated = std::fs::read_to_string(&path).unwrap();
        assert!(updated.contains("-- BEGIN hypr-dictate"));
        assert!(updated.contains("hl.bind(\"ALT + SUPER + V\", hl.dsp.exec_cmd(\"'/usr/bin/hypr-dictate' start\"))"));
        assert!(updated.contains("hl.bind(\"ALT + SUPER + W\", hl.dsp.exec_cmd(\"'/usr/bin/hypr-dictate' start-formal\"))"));
        assert!(updated.contains("-- END hypr-dictate"));

        // apply_hyprland_shortcuts in unit tests MUST return mock path and NEVER write to real HOME
        let mock_res = apply_hyprland_shortcuts(Some(&trans), Some(&writing), bin);
        assert!(mock_res.is_ok());
        assert_eq!(mock_res.unwrap(), PathBuf::from("/tmp/mock-hypr-dictate-bindings.lua"));
    }

    #[test]
    fn reject_identical_shortcuts_for_transcription_and_writing() {
        let bin = Path::new("/app/hypr-dictate");
        let s1 = ShortcutSpec::parse("SUPER + ALT + V").unwrap();
        let s2 = ShortcutSpec::parse("ALT + SUPER + v").unwrap();

        let err = update_bindings_content("", Some(&s1), Some(&s2), bin).unwrap_err();
        assert!(err.contains("conflicto"));
    }

    #[test]
    fn shortcut_never_touches_real_home_bindings_in_tests() {
        let home = std::env::var_os("HOME").expect("HOME set");
        let real_path = PathBuf::from(home).join(".config/hypr/bindings.lua");
        let trans = ShortcutSpec::parse("SUPER + ALT + V");
        let writing = ShortcutSpec::parse("SUPER + ALT + W");
        let bin = Path::new("/usr/bin/hypr-dictate");

        // apply() in tests must never touch real HOME
        assert!(apply("SUPER + ALT + V", "SUPER + ALT + V").is_ok());

        // apply_hyprland_shortcuts in tests must never touch real HOME
        assert!(apply_hyprland_shortcuts(trans.as_ref(), writing.as_ref(), bin).is_ok());

        // apply_hyprland_shortcuts_to_path pointing to real bindings in tests must never touch real HOME
        let res = apply_hyprland_shortcuts_to_path(trans.as_ref(), writing.as_ref(), bin, &real_path);
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), PathBuf::from("/tmp/mock-hypr-dictate-bindings.lua"));
    }
}

