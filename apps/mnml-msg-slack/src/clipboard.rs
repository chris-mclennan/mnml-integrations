//! Cross-platform clipboard write via `arboard`.

use anyhow::{Context, Result};

pub fn copy(text: &str) -> Result<()> {
    let mut cb = arboard::Clipboard::new().context("open system clipboard")?;
    cb.set_text(text.to_string())
        .context("write to system clipboard")?;
    Ok(())
}
