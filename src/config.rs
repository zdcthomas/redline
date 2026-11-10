//! `~/.redline/config.toml`. Every key is optional.

use serde::Deserialize;

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Popup {
    /// tmux popup inside tmux, embedded terminal otherwise.
    #[default]
    Auto,
    Tmux,
    Embedded,
    Fullscreen,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct Config {
    /// Editor command line, for example "nvim" or "hx". Default: $VISUAL, $EDITOR, vi.
    pub editor: Option<String>,
    pub popup: Popup,
    /// Name that the history shows for the human.
    pub author: Option<String>,
    /// Popup size in percent of the screen.
    pub popup_width: Option<u16>,
    pub popup_height: Option<u16>,
}

impl Config {
    pub fn load() -> Self {
        let path = crate::store::home().join("config.toml");
        let mut cfg: Config = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default();
        if let Ok(p) = std::env::var("REDLINE_POPUP") {
            cfg.popup = match p.as_str() {
                "tmux" => Popup::Tmux,
                "embedded" => Popup::Embedded,
                "fullscreen" => Popup::Fullscreen,
                _ => Popup::Auto,
            };
        }
        cfg
    }

    pub fn editor(&self) -> String {
        let env = |k: &str| std::env::var(k).ok().filter(|s| !s.trim().is_empty());
        env("REDLINE_EDITOR")
            .or_else(|| self.editor.clone())
            .or_else(|| env("VISUAL"))
            .or_else(|| env("EDITOR"))
            .unwrap_or_else(|| "vi".into())
    }

    pub fn author(&self) -> String {
        self.author
            .clone()
            .or_else(|| std::env::var("USER").ok())
            .unwrap_or_else(|| "human".into())
    }

    pub fn popup_pct(&self) -> (u16, u16) {
        (
            self.popup_width.unwrap_or(84).clamp(30, 100),
            self.popup_height.unwrap_or(84).clamp(30, 100),
        )
    }
}
