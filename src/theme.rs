//! State-local appearance only. No agent status, input, or identity decisions.
pub mod custom;

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
    Garage,
    Nebula,
    Void,
    Lab,
    Classic,
    Mono,
}
impl ThemeName {
    pub const ALL: [ThemeName; 7] = [
        Self::Portal,
        Self::Garage,
        Self::Nebula,
        Self::Void,
        Self::Lab,
        Self::Classic,
        Self::Mono,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Portal => "PORTAL",
            Self::Garage => "GARAGE",
            Self::Nebula => "NEBULA",
            Self::Void => "VOID",
            Self::Lab => "LAB",
            Self::Classic => "CLASSIC",
            Self::Mono => "MONO",
        }
    }
    /// One line for the theme picker.
    pub fn about(self) -> &'static str {
        match self {
            Self::Portal => "green, the colour physics turns when it gives up",
            Self::Garage => "amber workbench; the real science happens next to the lawnmower",
            Self::Nebula => "pink and violet, mildly radioactive",
            Self::Void => "deep blue; stare long enough and it asks for a code review",
            Self::Lab => "light, for people whose lab has windows",
            Self::Classic => "your terminal's own colours, untouched",
            Self::Mono => "no colour at all; very serious, very brave",
        }
    }
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|t| t.label().eq_ignore_ascii_case(name))
    }
    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|t| *t == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
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
                term_bg: Color::Rgb(13, 23, 20),
                term_fg: Color::Rgb(219, 231, 210),
                ansi: Some([
                    Color::Rgb(22, 35, 29),
                    Color::Rgb(255, 122, 107),
                    Color::Rgb(159, 227, 90),
                    Color::Rgb(242, 199, 107),
                    Color::Rgb(95, 179, 217),
                    Color::Rgb(199, 146, 234),
                    Color::Rgb(99, 213, 208),
                    Color::Rgb(207, 220, 200),
                    Color::Rgb(58, 84, 70),
                    Color::Rgb(255, 155, 143),
                    Color::Rgb(196, 245, 122),
                    Color::Rgb(255, 217, 143),
                    Color::Rgb(134, 203, 234),
                    Color::Rgb(219, 170, 242),
                    Color::Rgb(142, 230, 225),
                    Color::Rgb(240, 247, 234),
                ]),
            },
            Self::Garage => Palette {
                name: self,
                bg: Color::Rgb(23, 19, 15),
                panel: Color::Rgb(33, 27, 22),
                raised: Color::Rgb(48, 40, 31),
                text: Color::Rgb(236, 226, 210),
                muted: Color::Rgb(168, 152, 130),
                border: Color::Rgb(86, 72, 56),
                accent: Color::Rgb(255, 176, 59),
                cyan: Color::Rgb(110, 200, 190),
                warn: Color::Rgb(255, 214, 102),
                danger: Color::Rgb(255, 120, 96),
                ink: Color::Rgb(28, 20, 10),
                term_bg: Color::Rgb(27, 22, 18),
                term_fg: Color::Rgb(230, 220, 203),
                ansi: Some([
                    Color::Rgb(42, 34, 27),
                    Color::Rgb(232, 103, 79),
                    Color::Rgb(168, 198, 108),
                    Color::Rgb(240, 178, 71),
                    Color::Rgb(111, 166, 201),
                    Color::Rgb(196, 138, 192),
                    Color::Rgb(110, 200, 190),
                    Color::Rgb(216, 204, 184),
                    Color::Rgb(90, 76, 62),
                    Color::Rgb(255, 138, 112),
                    Color::Rgb(195, 220, 138),
                    Color::Rgb(255, 205, 107),
                    Color::Rgb(146, 194, 224),
                    Color::Rgb(219, 166, 214),
                    Color::Rgb(148, 221, 212),
                    Color::Rgb(247, 239, 226),
                ]),
            },
            Self::Nebula => Palette {
                name: self,
                bg: Color::Rgb(20, 15, 30),
                panel: Color::Rgb(28, 21, 42),
                raised: Color::Rgb(42, 31, 62),
                text: Color::Rgb(234, 226, 250),
                muted: Color::Rgb(160, 146, 190),
                border: Color::Rgb(78, 60, 110),
                accent: Color::Rgb(255, 110, 199),
                cyan: Color::Rgb(120, 220, 255),
                warn: Color::Rgb(255, 210, 120),
                danger: Color::Rgb(255, 110, 130),
                ink: Color::Rgb(30, 10, 30),
                term_bg: Color::Rgb(24, 18, 42),
                term_fg: Color::Rgb(228, 220, 246),
                ansi: Some([
                    Color::Rgb(38, 29, 56),
                    Color::Rgb(255, 110, 130),
                    Color::Rgb(143, 224, 168),
                    Color::Rgb(255, 210, 120),
                    Color::Rgb(122, 162, 255),
                    Color::Rgb(255, 110, 199),
                    Color::Rgb(120, 220, 255),
                    Color::Rgb(214, 205, 239),
                    Color::Rgb(78, 60, 110),
                    Color::Rgb(255, 148, 164),
                    Color::Rgb(176, 240, 196),
                    Color::Rgb(255, 226, 158),
                    Color::Rgb(163, 192, 255),
                    Color::Rgb(255, 158, 219),
                    Color::Rgb(165, 232, 255),
                    Color::Rgb(246, 241, 255),
                ]),
            },
            Self::Void => Palette {
                name: self,
                bg: Color::Rgb(11, 17, 30),
                panel: Color::Rgb(16, 24, 42),
                raised: Color::Rgb(25, 37, 62),
                text: Color::Rgb(220, 232, 250),
                muted: Color::Rgb(140, 160, 190),
                border: Color::Rgb(48, 68, 105),
                accent: Color::Rgb(92, 214, 255),
                cyan: Color::Rgb(130, 240, 210),
                warn: Color::Rgb(255, 206, 110),
                danger: Color::Rgb(255, 120, 130),
                ink: Color::Rgb(6, 20, 31),
                term_bg: Color::Rgb(14, 21, 36),
                term_fg: Color::Rgb(214, 226, 244),
                ansi: Some([
                    Color::Rgb(24, 35, 58),
                    Color::Rgb(255, 120, 130),
                    Color::Rgb(127, 224, 160),
                    Color::Rgb(255, 206, 110),
                    Color::Rgb(92, 156, 255),
                    Color::Rgb(196, 155, 255),
                    Color::Rgb(92, 214, 255),
                    Color::Rgb(205, 216, 234),
                    Color::Rgb(58, 77, 114),
                    Color::Rgb(255, 154, 162),
                    Color::Rgb(166, 240, 192),
                    Color::Rgb(255, 224, 154),
                    Color::Rgb(140, 188, 255),
                    Color::Rgb(220, 188, 255),
                    Color::Rgb(142, 230, 255),
                    Color::Rgb(242, 247, 255),
                ]),
            },
            Self::Lab => Palette {
                name: self,
                bg: Color::Rgb(246, 248, 244),
                panel: Color::Rgb(236, 240, 233),
                raised: Color::Rgb(222, 229, 218),
                text: Color::Rgb(24, 33, 27),
                muted: Color::Rgb(95, 110, 100),
                border: Color::Rgb(190, 200, 188),
                accent: Color::Rgb(46, 125, 50),
                cyan: Color::Rgb(0, 128, 140),
                warn: Color::Rgb(176, 110, 0),
                danger: Color::Rgb(196, 40, 40),
                ink: Color::Rgb(255, 255, 255),
                term_bg: Color::Rgb(251, 252, 250),
                term_fg: Color::Rgb(28, 37, 31),
                ansi: Some([
                    Color::Rgb(28, 37, 31),
                    Color::Rgb(196, 40, 40),
                    Color::Rgb(46, 125, 50),
                    Color::Rgb(154, 106, 0),
                    Color::Rgb(31, 95, 174),
                    Color::Rgb(138, 63, 160),
                    Color::Rgb(0, 128, 140),
                    Color::Rgb(216, 221, 213),
                    Color::Rgb(95, 110, 100),
                    Color::Rgb(224, 74, 74),
                    Color::Rgb(63, 154, 68),
                    Color::Rgb(185, 134, 0),
                    Color::Rgb(63, 127, 208),
                    Color::Rgb(168, 92, 192),
                    Color::Rgb(22, 154, 166),
                    Color::Rgb(255, 255, 255),
                ]),
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
                term_bg: Color::Reset,
                term_fg: Color::Reset,
                ansi: None,
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
                term_bg: Color::Reset,
                term_fg: Color::Reset,
                ansi: None,
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
    /// The terminal area of the pctl app: default background and text, and the 16 ANSI
    /// colours remapped to the theme (None keeps the terminal's own).
    pub term_bg: Color,
    pub term_fg: Color,
    pub ansi: Option<[Color; 16]>,
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
        let p = theme.palette();
        let hex = |c: Color| match c {
            Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
            _ => "default".to_string(),
        };
        let named = |v: [&str; 6]| v.map(String::from);
        let [bg, text, muted, accent, ink, alert] = match theme {
            ThemeName::Classic => named(["black", "white", "colour7", "cyan", "black", "red"]),
            ThemeName::Mono => named(["default"; 6]),
            _ => [p.bg, p.text, p.muted, p.accent, p.ink, p.danger].map(hex),
        };
        // Tabs show the agent's name and a state mark written by `session marks` (! blocked,
        // * working, x exited); the status line reruns it every 5 s while a client is attached.
        let name =
            "#{?#{==:#{window_name},manager},CONTROL,#{?#{@pctl_tab},#{@pctl_tab},#{window_name}}}";
        let mark = |base: &str| {
            format!(
                "#{{?#{{==:#{{@pctl_status}},blocked}},#[fg={alert}]!#[fg={base}] ,#{{?#{{==:#{{@pctl_status}},working}},* ,#{{?#{{==:#{{@pctl_status}},exited}},x ,}}}}}}"
            )
        };
        let marks_job = [
            std::env::current_exe()?.to_string_lossy().to_string(),
            "--state".into(),
            self.state.to_string_lossy().to_string(),
            "--tmux".into(),
            self.tmux.to_string_lossy().to_string(),
            "session".into(),
            "marks".into(),
        ]
        .iter()
        .map(|a| crate::shell_quote(a))
        .collect::<Vec<_>>()
        .join(" ");
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
                format!("#[{selected}] ◎ #{{?#{{@pctl_name}},#{{@pctl_name}},PCTL}} #[default] "),
            ),
            ("status-left-length", "40".into()),
            (
                "status-right",
                format!("#[fg={muted}] #({marks_job}) │ ^b s tree  m manager  q detach "),
            ),
            ("status-right-length", "80".into()),
            ("status-interval", "5".into()),
            (
                "window-status-format",
                format!("#[fg={muted}] #I:{}{name} ", mark(&muted)),
            ),
            (
                "window-status-current-format",
                format!("#[fg={accent},bold] ▸ #I:{}{name} ", mark(&accent)),
            ),
            ("pane-border-style", format!("fg={muted}")),
            ("pane-active-border-style", format!("fg={accent}")),
            ("message-style", format!("fg={text},bg={bg}")),
        ] {
            self.tm(&["set-option", "-g", key, &value])?;
        }
        // Workspace keys: ^b s shows projects and agents by name, ^b q detaches.
        let tree = format!(
            "#{{?window_format,{name} #{{@pctl_status}},#{{?session_format,#{{?#{{@pctl_name}},#{{@pctl_name}},#{{session_name}}}},#{{pane_current_command}}}}}}"
        );
        self.tm(&["bind-key", "s", "choose-tree", "-Zw", "-F", &tree])?;
        self.tm(&["bind-key", "q", "detach-client"])?;
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
