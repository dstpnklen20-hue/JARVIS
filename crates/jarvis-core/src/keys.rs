//! Имена клавиш в манифестах → виртуальные коды Windows.
//!
//! Скан-код для конкретной раскладки и признак «расширенной» клавиши
//! вычисляет платформа (MapVirtualKey) — так нажатия доходят и до игр.

/// Виртуальный код клавиши по имени (без учёта регистра): «a», «f5»,
/// «ctrl», «space», «up»… `None` — такой клавиши нет.
pub fn vk_code(name: &str) -> Option<u16> {
    let name = name.trim().to_ascii_lowercase();
    let code = match name.as_str() {
        "backspace" => 0x08,
        "tab" => 0x09,
        "enter" | "return" => 0x0D,
        "shift" | "lshift" => 0xA0,
        "rshift" => 0xA1,
        "ctrl" | "control" | "lctrl" => 0xA2,
        "rctrl" => 0xA3,
        "alt" | "lalt" => 0xA4,
        "ralt" | "altgr" => 0xA5,
        "pause" => 0x13,
        "capslock" => 0x14,
        "esc" | "escape" => 0x1B,
        "space" => 0x20,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "end" => 0x23,
        "home" => 0x24,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "printscreen" => 0x2C,
        "insert" | "ins" => 0x2D,
        "delete" | "del" => 0x2E,
        "win" | "lwin" => 0x5B,
        "rwin" => 0x5C,
        "menu" | "apps" => 0x5D,
        "num0" => 0x60,
        "num1" => 0x61,
        "num2" => 0x62,
        "num3" => 0x63,
        "num4" => 0x64,
        "num5" => 0x65,
        "num6" => 0x66,
        "num7" => 0x67,
        "num8" => 0x68,
        "num9" => 0x69,
        "num*" | "multiply" => 0x6A,
        "num+" | "add" => 0x6B,
        "num-" | "subtract" => 0x6D,
        "num." | "decimal" => 0x6E,
        "num/" | "divide" => 0x6F,
        "numlock" => 0x90,
        "scrolllock" => 0x91,
        ";" => 0xBA,
        "=" => 0xBB,
        "," => 0xBC,
        "-" => 0xBD,
        "." => 0xBE,
        "/" => 0xBF,
        "`" => 0xC0,
        "[" => 0xDB,
        "\\" => 0xDC,
        "]" => 0xDD,
        "'" => 0xDE,
        // Мыши в SendInput клавиатурой не нажать — для игр это кнопки мыши,
        // их добавим, если понадобятся профилям (этап 6).
        _ => {
            let mut chars = name.chars();
            match (chars.next(), chars.as_str()) {
                (Some(c), "") if c.is_ascii_alphanumeric() => c.to_ascii_uppercase() as u16,
                (Some('f'), n) => match n.parse::<u16>() {
                    Ok(n @ 1..=24) => 0x70 + n - 1,
                    _ => return None,
                },
                _ => return None,
            }
        }
    };
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_digits_function_keys() {
        assert_eq!(vk_code("a"), Some(0x41));
        assert_eq!(vk_code("Z"), Some(0x5A));
        assert_eq!(vk_code("7"), Some(0x37));
        assert_eq!(vk_code("F1"), Some(0x70));
        assert_eq!(vk_code("f24"), Some(0x87));
    }

    #[test]
    fn named_keys() {
        assert_eq!(vk_code("ctrl"), Some(0xA2));
        assert_eq!(vk_code("Space"), Some(0x20));
        assert_eq!(vk_code("esc"), Some(0x1B));
        assert_eq!(vk_code("up"), Some(0x26));
        assert_eq!(vk_code("num5"), Some(0x65));
    }

    #[test]
    fn unknown_keys() {
        for bad in ["", "f25", "ctrl+a", "банан", "ab", "mouse1"] {
            assert_eq!(vk_code(bad), None, "{bad}");
        }
    }
}
