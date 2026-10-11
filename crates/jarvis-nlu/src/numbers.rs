//! Числа словами → цифры: «сорок пять» → «45», «двести три» → «203».
//!
//! Распознаватель пишет числа словами; команды со слотами (этап 4) и
//! сравнение фраз удобнее вести в цифрах. Поддерживаются 0–999 999 и
//! основные падежные формы («пяти», «сорока», «одну»).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    /// 0–9
    Unit(u32),
    /// 10–19
    Teen(u32),
    /// 20, 30, … 90
    Ten(u32),
    /// 100, 200, … 900
    Hundred(u32),
    Thousand,
}

fn part(word: &str) -> Option<Part> {
    use Part::*;
    Some(match word {
        "ноль" | "нуль" | "нуля" => Unit(0),
        "один" | "одна" | "одно" | "одну" | "одного" | "одной" | "одному" | "одним" => {
            Unit(1)
        }
        "два" | "две" | "двух" | "двум" | "двумя" => Unit(2),
        "три" | "трех" | "трем" | "тремя" => Unit(3),
        "четыре" | "четырех" | "четырем" | "четырьмя" => Unit(4),
        "пять" | "пяти" | "пятью" => Unit(5),
        "шесть" | "шести" | "шестью" => Unit(6),
        "семь" | "семи" | "семью" => Unit(7),
        "восемь" | "восьми" | "восемью" => Unit(8),
        "девять" | "девяти" | "девятью" => Unit(9),
        "десять" | "десяти" => Teen(10),
        "одиннадцать" | "одиннадцати" => Teen(11),
        "двенадцать" | "двенадцати" => Teen(12),
        "тринадцать" | "тринадцати" => Teen(13),
        "четырнадцать" | "четырнадцати" => Teen(14),
        "пятнадцать" | "пятнадцати" => Teen(15),
        "шестнадцать" | "шестнадцати" => Teen(16),
        "семнадцать" | "семнадцати" => Teen(17),
        "восемнадцать" | "восемнадцати" => Teen(18),
        "девятнадцать" | "девятнадцати" => Teen(19),
        "двадцать" | "двадцати" => Ten(20),
        "тридцать" | "тридцати" => Ten(30),
        "сорок" | "сорока" => Ten(40),
        "пятьдесят" | "пятидесяти" => Ten(50),
        "шестьдесят" | "шестидесяти" => Ten(60),
        "семьдесят" | "семидесяти" => Ten(70),
        "восемьдесят" | "восьмидесяти" => Ten(80),
        "девяносто" | "девяноста" => Ten(90),
        "сто" | "ста" => Hundred(100),
        "двести" | "двухсот" => Hundred(200),
        "триста" | "трехсот" => Hundred(300),
        "четыреста" | "четырехсот" => Hundred(400),
        "пятьсот" | "пятисот" => Hundred(500),
        "шестьсот" | "шестисот" => Hundred(600),
        "семьсот" | "семисот" => Hundred(700),
        "восемьсот" | "восьмисот" => Hundred(800),
        "девятьсот" | "девятисот" => Hundred(900),
        "тысяча" | "тысячи" | "тысяч" | "тысячу" => Thousand,
        _ => return None,
    })
}

/// Накопитель одного числа: следит, чтобы части шли по убыванию разрядов
/// («двадцать пять» — одно число, «пять двадцать» — два).
#[derive(Debug, Default)]
struct Number {
    thousands: Option<u32>,
    /// Текущая группа (до тысяч) и самый мелкий занятый разряд в ней.
    group: u32,
    last: Option<u8>,
    started: bool,
}

impl Number {
    /// Порядок разряда: сотни 3, десятки 2, «надцать» 1.5 → 2, единицы 1.
    fn rank(p: Part) -> u8 {
        match p {
            Part::Hundred(_) => 3,
            Part::Ten(_) | Part::Teen(_) => 2,
            Part::Unit(_) => 1,
            Part::Thousand => 4,
        }
    }

    /// Можно ли добавить часть к текущему числу.
    fn accepts(&self, p: Part) -> bool {
        if !self.started {
            return true;
        }
        match p {
            // «тысяча» — после группы (или сама по себе), один раз.
            Part::Thousand => self.thousands.is_none(),
            _ => {
                // После «надцати» и после единиц продолжать нельзя.
                let last = self.last.unwrap_or(4);
                Self::rank(p) < last && !matches!(self.last, Some(1))
            }
        }
    }

    fn push(&mut self, p: Part) {
        self.started = true;
        match p {
            Part::Thousand => {
                self.thousands = Some(if self.group == 0 { 1 } else { self.group });
                self.group = 0;
                self.last = None;
            }
            Part::Teen(v) => {
                self.group += v;
                // «надцать» закрывает и десятки, и единицы.
                self.last = Some(1);
            }
            Part::Unit(v) | Part::Ten(v) | Part::Hundred(v) => {
                self.group += v;
                self.last = Some(Self::rank(p));
            }
        }
    }

    fn value(&self) -> u32 {
        self.thousands.unwrap_or(0) * 1000 + self.group
    }
}

/// Заменяет числа словами на цифры. Слова — уже в нижнем регистре, ё→е.
pub fn words_to_digits(words: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(words.len());
    let mut current: Option<Number> = None;
    for word in words {
        match part(word) {
            Some(p) => {
                let number = current.get_or_insert_with(Number::default);
                if !number.accepts(p) {
                    out.push(number.value().to_string());
                    *number = Number::default();
                }
                number.push(p);
            }
            None => {
                if let Some(n) = current.take() {
                    out.push(n.value().to_string());
                }
                out.push(word.clone());
            }
        }
    }
    if let Some(n) = current {
        out.push(n.value().to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(s: &str) -> String {
        let words: Vec<String> = s.split_whitespace().map(str::to_owned).collect();
        words_to_digits(&words).join(" ")
    }

    #[test]
    fn simple_numbers() {
        assert_eq!(conv("громкость на сорок"), "громкость на 40");
        assert_eq!(conv("ноль"), "0");
        assert_eq!(conv("пятнадцать минут"), "15 минут");
        assert_eq!(conv("сто"), "100");
    }

    #[test]
    fn compound_numbers() {
        assert_eq!(conv("сорок пять"), "45");
        assert_eq!(conv("двести три"), "203");
        assert_eq!(conv("девятьсот девяносто девять"), "999");
        assert_eq!(conv("сто двенадцать"), "112");
        assert_eq!(conv("две тысячи двадцать шесть"), "2026");
        assert_eq!(conv("тысяча"), "1000");
    }

    #[test]
    fn oblique_forms() {
        assert_eq!(conv("таймер на пять минут"), "таймер на 5 минут");
        assert_eq!(conv("до сорока пяти процентов"), "до 45 процентов");
        assert_eq!(conv("на одну минуту"), "на 1 минуту");
    }

    #[test]
    fn wrong_order_splits_numbers() {
        assert_eq!(conv("пять двадцать"), "5 20");
        assert_eq!(conv("двадцать сорок"), "20 40");
        assert_eq!(conv("двенадцать три"), "12 3");
        assert_eq!(conv("один два три"), "1 2 3");
    }

    #[test]
    fn non_numbers_untouched() {
        assert_eq!(conv("открой ютуб"), "открой ютуб");
        assert_eq!(conv(""), "");
    }
}
