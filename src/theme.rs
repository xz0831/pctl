//! State-local appearance only. No agent status, input, or identity decisions.
use crate::{Core, atomic_json};
use anyhow::{Result, ensure};
use ratatui::{
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, BorderType, Borders},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum ThemeName {
    #[default]
    Portal,
    Classic,
    Mono,
}
impl ThemeName {
    pub fn label(self) -> &'static str {
        match self {
            Self::Portal => "PORTAL",
            Self::Classic => "CLASSIC",
            Self::Mono => "MONO",
        }
    }
    pub fn palette(self) -> Palette {
        match self {
            Self::Portal => Palette {
                name: self,
                bg: Color::Rgb(10, 19, 17),
                panel: Color::Rgb(16, 29, 24),
                raised: Color::Rgb(24, 42, 34),
                text: Color::Rgb(229, 239, 218),
                muted: Color::Rgb(143, 168, 150),
                border: Color::Rgb(50, 85, 63),
                accent: Color::Rgb(183, 244, 94),
                cyan: Color::Rgb(99, 213, 208),
                warn: Color::Rgb(245, 199, 107),
                danger: Color::Rgb(255, 136, 120),
                ink: Color::Rgb(12, 25, 16),
            },
            Self::Classic => Palette {
                name: self,
                bg: Color::Black,
                panel: Color::Black,
                raised: Color::Black,
                text: Color::White,
                muted: Color::Gray,
                border: Color::DarkGray,
                accent: Color::Cyan,
                cyan: Color::LightCyan,
                warn: Color::LightYellow,
                danger: Color::LightRed,
                ink: Color::Black,
            },
            Self::Mono => Palette {
                name: self,
                bg: Color::Reset,
                panel: Color::Reset,
                raised: Color::Reset,
                text: Color::Reset,
                muted: Color::Reset,
                border: Color::Reset,
                accent: Color::Reset,
                cyan: Color::Reset,
                warn: Color::Reset,
                danger: Color::Reset,
                ink: Color::Reset,
            },
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub name: ThemeName,
    pub bg: Color,
    pub panel: Color,
    pub raised: Color,
    pub text: Color,
    pub muted: Color,
    pub border: Color,
    pub accent: Color,
    pub cyan: Color,
    pub warn: Color,
    pub danger: Color,
    pub ink: Color,
}
impl Palette {
    pub fn base(self) -> Style {
        Style::default().fg(self.text).bg(self.bg)
    }
    pub fn body(self) -> Style {
        Style::default().fg(self.text).bg(self.panel)
    }
    pub fn muted(self) -> Style {
        self.body().fg(self.muted)
    }
    pub fn accent(self) -> Style {
        self.body().fg(self.accent).add_modifier(Modifier::BOLD)
    }
    pub fn selected(self) -> Style {
        if self.name == ThemeName::Mono {
            self.body()
                .add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else if self.name == ThemeName::Classic {
            // Some 16-color terminals brighten bold black into unreadable gray.
            Style::default().fg(self.ink).bg(self.accent)
        } else {
            Style::default()
                .fg(self.ink)
                .bg(self.accent)
                .add_modifier(Modifier::BOLD)
        }
    }
    pub fn block<'a>(self, title: impl Into<Line<'a>>) -> Block<'a> {
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(self.body())
            .border_style(self.body().fg(self.border))
            .title_style(self.accent())
            .title(title)
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Appearance {
    theme: ThemeName,
}
impl Core {
    pub fn theme(&self) -> Result<ThemeName> {
        let path = self.state.join("appearance.json");
        if !path.exists() {
            return Ok(ThemeName::default());
        }
        ensure!(
            std::fs::metadata(&path)?.len() <= 4096,
            "appearance config exceeds read limit"
        );
        Ok(serde_json::from_slice::<Appearance>(&std::fs::read(path)?)?.theme)
    }
    pub fn set_theme(&self, theme: ThemeName) -> Result<()> {
        let _lock = self.lock()?;
        if self.state.join("tmux.sock").exists() {
            ensure!(
                self.server()? == self.registry()?.server_instance,
                "server identity changed; theme locked"
            );
        }
        atomic_json(&self.state.join("appearance.json"), &Appearance { theme })?;
        if self.state.join("tmux.sock").exists() {
            self.apply_theme_chrome()?;
        }
        Ok(())
    }
    pub(crate) fn apply_theme_chrome(&self) -> Result<()> {
        let theme = self.theme()?;
        let (bg, text, muted, accent, ink) = match theme {
            ThemeName::Portal => ("#0a1311", "#e5efda", "#8fa896", "#b7f45e", "#0c1910"),
            ThemeName::Classic => ("black", "white", "colour7", "cyan", "black"),
            ThemeName::Mono => ("default", "default", "default", "default", "default"),
        };
        let selected = if theme == ThemeName::Mono {
            "reverse,bold".to_string()
        } else if theme == ThemeName::Classic {
            format!("fg={ink},bg={accent}")
        } else {
            format!("fg={ink},bg={accent},bold")
        };
        for (key, value) in [
            ("status-style", format!("bg={bg},fg={text}")),
            (
                "status-left",
                format!("#[{selected}] ◎ PCTL / {} #[default] ", theme.label()),
            ),
            ("status-left-length", "26".into()),
            (
                "status-right",
                format!("#[fg={muted}] ^b m manager  │  ^b d detach "),
            ),
            ("status-right-length", "36".into()),
            ("status-interval", "0".into()),
            (
                "window-status-format",
                format!(
                    "#[fg={muted}] #I:#{name} ",
                    name = "{?#{==:#{window_name},manager},CONTROL,#{window_name}}"
                ),
            ),
            (
                "window-status-current-format",
                format!(
                    "#[fg={accent},bold] ▸ #I:#{name} ",
                    name = "{?#{==:#{window_name},manager},CONTROL,#{window_name}}"
                ),
            ),
            ("pane-border-style", format!("fg={muted}")),
            ("pane-active-border-style", format!("fg={accent}")),
            ("message-style", format!("fg={text},bg={bg}")),
        ] {
            self.tm(&["set-option", "-g", key, &value])?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn presets_are_bounded_and_persist_in_state_only() {
        let root = tempfile::tempdir().unwrap();
        let c = Core::new(root.path().join("state"), "/not/tmux".into()).unwrap();
        assert_eq!(c.theme().unwrap(), ThemeName::Portal);
        for theme in [ThemeName::Classic, ThemeName::Mono, ThemeName::Portal] {
            c.set_theme(theme).unwrap();
            assert_eq!(c.theme().unwrap(), theme);
        }
        std::fs::write(c.state.join("appearance.json"), r#"{"theme":"execute"}"#).unwrap();
        assert!(c.theme().is_err());
        std::fs::write(
            c.state.join("appearance.json"),
            r#"{"theme":"portal","command":"sh"}"#,
        )
        .unwrap();
        assert!(c.theme().is_err());
        assert!(c.registry().unwrap().runs.is_empty());
    }
}
