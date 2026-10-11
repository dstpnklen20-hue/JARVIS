//! Значок в трее: индикатор состояния и меню «Пауза» / «Забыть разрешения» / «Выход».
//!
//! Работает в главном потоке, где крутится цикл сообщений Windows.

use anyhow::Context as _;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

/// Что показывает значок.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Indicator {
    /// Модели загружаются.
    Starting,
    /// Ждёт «Джарвис».
    Idle,
    /// Слушает команду.
    Listening,
    /// Ждёт ответа на вопрос-разрешение.
    Asking,
    Paused,
    /// Слух не работает (нет микрофона, не загрузились модели).
    Problem(String),
}

impl Indicator {
    fn color(&self) -> [u8; 3] {
        match self {
            Indicator::Starting => [0xE0, 0xA0, 0x20],
            Indicator::Idle => [0x2E, 0xB8, 0x5C],
            Indicator::Listening => [0x2F, 0x80, 0xED],
            Indicator::Asking => [0x9B, 0x51, 0xE0],
            Indicator::Paused => [0x8A, 0x8F, 0x98],
            Indicator::Problem(_) => [0xE0, 0x3E, 0x3E],
        }
    }

    fn tooltip(&self) -> String {
        let text = match self {
            Indicator::Starting => "загружаюсь…".to_owned(),
            Indicator::Idle => "жду «Джарвис»".to_owned(),
            Indicator::Listening => "слушаю, сэр".to_owned(),
            Indicator::Asking => "жду вашего решения".to_owned(),
            Indicator::Paused => "на паузе".to_owned(),
            Indicator::Problem(why) => format!("не слышу: {why}"),
        };
        // Подсказка трея в Windows ограничена 127 символами.
        let full = format!("Джарвис — {text}");
        full.chars().take(120).collect()
    }
}

/// Действие пользователя в меню.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    TogglePause,
    ForgetPermissions,
    ReloadCommands,
    Quit,
}

pub struct Tray {
    icon: TrayIcon,
    pause: CheckMenuItem,
    forget: MenuItem,
    reload: MenuItem,
    quit: MenuItem,
    shown: Indicator,
}

impl Tray {
    pub fn new() -> anyhow::Result<Self> {
        let pause = CheckMenuItem::new("Пауза", true, false, None);
        let reload = MenuItem::new("Обновить команды", true, None);
        let forget = MenuItem::new("Забыть разрешения", true, None);
        let quit = MenuItem::new("Выход", true, None);
        let menu = Menu::new();
        menu.append_items(&[
            &pause,
            &reload,
            &forget,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .context("не удалось создать меню трея")?;
        let shown = Indicator::Starting;
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(shown.tooltip())
            .with_icon(render_icon(shown.color())?)
            .build()
            .context("не удалось создать значок в трее")?;
        Ok(Self {
            icon,
            pause,
            reload,
            forget,
            quit,
            shown,
        })
    }

    pub fn show(&mut self, indicator: Indicator) {
        if indicator == self.shown {
            return;
        }
        let result = render_icon(indicator.color()).and_then(|icon| {
            self.icon.set_icon(Some(icon))?;
            self.icon.set_tooltip(Some(indicator.tooltip()))?;
            Ok(())
        });
        if let Err(e) = result {
            tracing::warn!(error = %e, "не удалось обновить значок трея");
        }
        self.pause.set_checked(indicator == Indicator::Paused);
        self.shown = indicator;
    }

    /// Следующее действие из меню, если пользователь что-то выбрал.
    pub fn poll_menu(&self) -> Option<MenuAction> {
        let event = MenuEvent::receiver().try_recv().ok()?;
        if event.id == *self.pause.id() {
            Some(MenuAction::TogglePause)
        } else if event.id == *self.reload.id() {
            Some(MenuAction::ReloadCommands)
        } else if event.id == *self.forget.id() {
            Some(MenuAction::ForgetPermissions)
        } else if event.id == *self.quit.id() {
            Some(MenuAction::Quit)
        } else {
            None
        }
    }
}

const SIZE: u32 = 32;

/// Круг с тёмной обводкой и сглаженным краем.
fn render_icon(color: [u8; 3]) -> anyhow::Result<Icon> {
    Icon::from_rgba(icon_pixels(color), SIZE, SIZE).context("не удалось нарисовать значок")
}

fn icon_pixels([r, g, b]: [u8; 3]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    let c = (SIZE as f32 - 1.0) / 2.0;
    let radius = SIZE as f32 / 2.0 - 1.5;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            let alpha = (radius + 0.5 - d).clamp(0.0, 1.0);
            // Обводка: последние 2.5 px темнее.
            let shade = if d > radius - 2.5 { 0.6 } else { 1.0 };
            let px = |v: u8| (f32::from(v) * shade) as u8;
            rgba.extend_from_slice(&[px(r), px(g), px(b), (alpha * 255.0) as u8]);
        }
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_is_opaque_inside_and_transparent_in_corners() {
        let px = icon_pixels([10, 200, 30]);
        assert_eq!(px.len(), (SIZE * SIZE * 4) as usize);
        let at = |x: u32, y: u32| &px[((y * SIZE + x) * 4) as usize..][..4];
        assert_eq!(at(0, 0)[3], 0);
        assert_eq!(at(SIZE / 2, SIZE / 2), [10, 200, 30, 255]);
    }

    #[test]
    fn tooltips_fit_windows_limit() {
        let long = Indicator::Problem("очень длинная причина ".repeat(20));
        assert!(long.tooltip().chars().count() <= 120);
        assert_eq!(Indicator::Idle.tooltip(), "Джарвис — жду «Джарвис»");
    }

    #[test]
    fn every_state_has_its_own_color() {
        let colors = [
            Indicator::Starting.color(),
            Indicator::Idle.color(),
            Indicator::Listening.color(),
            Indicator::Asking.color(),
            Indicator::Paused.color(),
            Indicator::Problem(String::new()).color(),
        ];
        for (i, a) in colors.iter().enumerate() {
            for b in &colors[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}
