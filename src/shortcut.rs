use crate::{hypr, settings::binding_block};
use anyhow::{Context, Result, ensure};
use std::io::Write;

pub fn apply(shortcut: &str, old: &str) -> Result<()> {
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
    let path = std::path::PathBuf::from(std::env::var_os("HOME").context("Falta HOME")?)
        .join(".config/hypr/bindings.lua");
    for p in path.ancestors() {
        ensure!(
            !std::fs::symlink_metadata(p)?.file_type().is_symlink(),
            "Symlink inesperado en configuración Hyprland"
        );
    }
    let original = std::fs::read_to_string(&path)?;
    let exe = std::env::current_exe()?;
    let updated = binding_block(&original, shortcut, exe.to_str().context("Ruta no UTF-8")?)?;
    let mut temporary = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    temporary
        .as_file()
        .set_permissions(std::fs::metadata(&path)?.permissions())?;
    temporary.write_all(updated.as_bytes())?;
    temporary.as_file().sync_all()?;
    let syntax = std::process::Command::new("luac")
        .arg("-p")
        .arg(temporary.path())
        .output()
        .context("Necesitas luac para validar el atajo antes de guardarlo")?;
    ensure!(
        syntax.status.success(),
        "El bloque generado no pasa la validación Lua"
    );
    ensure!(
        std::fs::read_to_string(&path)? == original,
        "La configuración cambió mientras se guardaba"
    );
    temporary.persist(&path)?;
    if let Err(error) = hypr(&["reload", "config-only"]) {
        std::fs::write(&path, original)?;
        return Err(error);
    }
    let errors = hypr(&["configerrors"])?;
    if !String::from_utf8_lossy(&errors).trim().is_empty() {
        std::fs::write(&path, original)?;
        let _ = hypr(&["reload", "config-only"]);
        anyhow::bail!("Hyprland rechazó el cambio; se restauró el archivo anterior");
    }
    Ok(())
}
