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
            // Свой цвет реактора — спокойный голубой; слушает — ярче, почти белый.
            Indicator::Idle => [0x1E, 0x8C, 0xF0],
            Indicator::Listening => [0x7F, 0xF4, 0xFF],
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
    ToggleAutostart,
    Quit,
}

pub struct Tray {
    icon: TrayIcon,
    pause: CheckMenuItem,
    forget: MenuItem,
    reload: MenuItem,
    autostart: CheckMenuItem,
    quit: MenuItem,
    shown: Indicator,
}

impl Tray {
    /// `autostart` — включён ли сейчас запуск при входе в Windows.
    pub fn new(autostart_on: bool) -> anyhow::Result<Self> {
        let pause = CheckMenuItem::new("Пауза", true, false, None);
        let autostart =
            CheckMenuItem::new("Запускать при входе в Windows", true, autostart_on, None);
        let reload = MenuItem::new("Обновить команды", true, None);
        let forget = MenuItem::new("Забыть разрешения", true, None);
        let quit = MenuItem::new("Выход", true, None);
        let menu = Menu::new();
        menu.append_items(&[
            &pause,
            &reload,
            &forget,
            &autostart,
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
            autostart,
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

    /// Показать состояние автозапуска (после переключения или ошибки).
    pub fn set_autostart(&self, on: bool) {
        self.autostart.set_checked(on);
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
        } else if event.id == *self.autostart.id() {
            Some(MenuAction::ToggleAutostart)
        } else if event.id == *self.quit.id() {
            Some(MenuAction::Quit)
        } else {
            None
        }
    }
}

const SIZE: u32 = 32;
/// Подвыборок на пиксель по каждой оси — сглаживание краёв.
const SUPERSAMPLE: u32 = 4;
/// Тёмных сегментов на внешнем кольце реактора.
const SEGMENTS: f32 = 10.0;

/// Дуговой реактор: светящееся кольцо с тёмными сегментами, зазор, ядро.
/// Свечение окрашено в цвет состояния.
fn render_icon(color: [u8; 3]) -> anyhow::Result<Icon> {
    Icon::from_rgba(icon_pixels(color), SIZE, SIZE).context("не удалось нарисовать значок")
}

fn icon_pixels(color: [u8; 3]) -> Vec<u8> {
    let tint = color.map(|v| f32::from(v) / 255.0);
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    let half = SIZE as f32 / 2.0;
    let step = 1.0 / SUPERSAMPLE as f32;
    for y in 0..SIZE {
        for x in 0..SIZE {
            // Среднее по подвыборкам с предумноженной прозрачностью.
            let mut sum = [0.0f32; 4];
            for sy in 0..SUPERSAMPLE {
                for sx in 0..SUPERSAMPLE {
                    let px = x as f32 + (sx as f32 + 0.5) * step;
                    let py = y as f32 + (sy as f32 + 0.5) * step;
                    if let Some(rgb) = reactor((px - half) / half, (py - half) / half, tint) {
                        for (s, v) in sum.iter_mut().zip(rgb) {
                            *s += v;
                        }
                        sum[3] += 1.0;
                    }
                }
            }
            let n = (SUPERSAMPLE * SUPERSAMPLE) as f32;
            let alpha = sum[3] / n;
            let channel = |v: f32| {
                if sum[3] > 0.0 {
                    (v / sum[3] * 255.0).round().clamp(0.0, 255.0) as u8
                } else {
                    0
                }
            };
            rgba.extend_from_slice(&[
                channel(sum[0]),
                channel(sum[1]),
                channel(sum[2]),
                (alpha * 255.0).round() as u8,
            ]);
        }
    }
    rgba
}

/// Цвет реактора в точке (`x`, `y` от −1 до 1, центр — 0); `None` — снаружи.
fn reactor(x: f32, y: f32, tint: [f32; 3]) -> Option<[f32; 3]> {
    let r = (x * x + y * y).sqrt();
    let dark = |k: f32| tint.map(|c| c * k);
    let light = |k: f32| tint.map(|c| c + (1.0 - c) * k);
    // Сегменты: 10 штук, один сверху, занимают 45 % окружности; лежат внутри
    // светящегося кольца — свечение обрамляет их с обеих сторон.
    let turn = (x.atan2(-y) / std::f32::consts::TAU * SEGMENTS + 0.5).rem_euclid(1.0);
    let segment = (turn - 0.5).abs() < 0.225;
    let rgb = match r {
        r if r > 1.0 => return None,
        // Тонкий тёмный обод — значок читается и на светлой панели задач.
        r if r > 0.94 => dark(0.35),
        r if r > 0.66 && r < 0.87 && segment => dark(0.22),
        r if r > 0.58 => light(0.3),
        r if r > 0.47 => dark(0.18),
        r if r > 0.36 => light(0.15),
        r if r > 0.27 => dark(0.45),
        _ => light(0.8),
    };
    Some(rgb)
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
        // Ядро светлее цвета состояния, но того же оттенка.
        let core = at(SIZE / 2, SIZE / 2);
        assert_eq!(core[3], 255);
        assert!(core[1] > 200 && core[0] > 10 && core[1] > core[0]);
    }

    #[test]
    fn reactor_ring_alternates_segments_and_glow() {
        let tint = [0.1, 0.5, 0.9];
        let ring = 0.74;
        // Сверху — тёмный сегмент, между сегментами (18° в сторону) — свечение.
        let top = reactor(0.0, -ring, tint).unwrap();
        let gap = std::f32::consts::PI / SEGMENTS;
        let between = reactor(ring * gap.sin(), -ring * gap.cos(), tint).unwrap();
        assert!(top[2] < 0.5 && between[2] > 0.9);
        assert!(reactor(0.8, 0.8, tint).is_none());
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
