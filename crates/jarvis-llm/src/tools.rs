//! Команды Джарвиса как инструмент модели: один инструмент `run_command`
//! с перечнем допустимых id. Модель только выбирает команду — выполняет её
//! ядро, через ту же проверку разрешений, что и голосовую.

use serde_json::{Value, json};

use crate::turn::ToolCall;

pub const TOOL_NAME: &str = "run_command";

/// Команда, доступная модели.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCommand {
    pub id: String,
    /// Что сделает («откроет YouTube в браузере»).
    pub description: String,
    /// Как её обычно говорят («открой ютуб»).
    pub example: String,
    /// Описание параметра, если команда его принимает («уровень громкости 0–100»).
    pub param: Option<String>,
}

/// Что модель попросила выполнить.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRequest {
    pub tool_use_id: String,
    pub command: String,
    /// Значение параметра как строка; проверяет его ядро по манифесту.
    pub value: Option<String>,
}

/// Описание инструмента для запроса. Порядок команд — как в списке
/// (он стабилен, иначе кэш подсказки сбрасывался бы).
pub fn tool_definition(commands: &[ToolCommand]) -> Value {
    let list: Vec<String> = commands
        .iter()
        .map(|c| {
            let param = c
                .param
                .as_ref()
                .map(|p| format!("; параметр value: {p}"))
                .unwrap_or_default();
            format!(
                "- {}: {} (говорят «{}»){param}",
                c.id, c.description, c.example
            )
        })
        .collect();
    let ids: Vec<&str> = commands.iter().map(|c| c.id.as_str()).collect();
    json!({
        "name": TOOL_NAME,
        "description": format!(
            "Выполнить команду на компьютере пользователя. Только когда он прямо просит что-то \
             сделать (открыть, запустить, громкость и т. п.), и только из этого списка; \
             перед выполнением Джарвис может спросить у пользователя разрешение.\n{}",
            list.join("\n")
        ),
        "input_schema": {
            "type": "object",
            "properties": {
                "command": {"type": "string", "enum": ids, "description": "id команды из списка"},
                "value": {"type": "string", "description": "значение параметра, если у команды он есть"}
            },
            "required": ["command"]
        },
        "eager_input_streaming": true
    })
}

/// Проверка вызова: инструмент наш, команда из списка, параметры разборчивы.
/// `Err` — текст для `tool_result` с `is_error`.
pub fn parse_call(call: &ToolCall, commands: &[ToolCommand]) -> Result<CommandRequest, String> {
    if call.name != TOOL_NAME {
        return Err(format!("неизвестный инструмент {}", call.name));
    }
    let input = call
        .input
        .as_ref()
        .ok_or_else(|| "параметры вызова неразборчивы".to_owned())?;
    let command = input["command"]
        .as_str()
        .ok_or_else(|| "нет поля command".to_owned())?;
    if !commands.iter().any(|c| c.id == command) {
        return Err(format!("команды {command} нет в списке"));
    }
    let value = match &input["value"] {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        other => return Err(format!("value должно быть строкой, пришло {other}")),
    };
    Ok(CommandRequest {
        tool_use_id: call.id.clone(),
        command: command.to_owned(),
        value,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands() -> Vec<ToolCommand> {
        vec![
            ToolCommand {
                id: "open_youtube".into(),
                description: "откроет YouTube".into(),
                example: "открой ютуб".into(),
                param: None,
            },
            ToolCommand {
                id: "media_volume_set".into(),
                description: "установит громкость".into(),
                example: "громкость на".into(),
                param: Some("уровень громкости 0–100".into()),
            },
        ]
    }

    fn call(input: Value) -> ToolCall {
        ToolCall {
            id: "t1".into(),
            name: TOOL_NAME.into(),
            input: Some(input),
        }
    }

    #[test]
    fn definition_lists_commands_and_ids() {
        let def = tool_definition(&commands());
        assert_eq!(def["name"], TOOL_NAME);
        assert_eq!(
            def["input_schema"]["properties"]["command"]["enum"],
            json!(["open_youtube", "media_volume_set"])
        );
        let text = def["description"].as_str().unwrap();
        assert!(text.contains("open_youtube: откроет YouTube"), "{text}");
        assert!(text.contains("параметр value: уровень громкости"), "{text}");
    }

    #[test]
    fn valid_calls_are_accepted() {
        let c = commands();
        assert_eq!(
            parse_call(
                &call(json!({"command": "media_volume_set", "value": 40})),
                &c
            ),
            Ok(CommandRequest {
                tool_use_id: "t1".into(),
                command: "media_volume_set".into(),
                value: Some("40".into()),
            })
        );
        assert_eq!(
            parse_call(&call(json!({"command": "open_youtube"})), &c)
                .unwrap()
                .value,
            None
        );
    }

    #[test]
    fn foreign_or_broken_calls_are_rejected() {
        let c = commands();
        assert!(parse_call(&call(json!({"command": "rm_rf"})), &c).is_err());
        assert!(parse_call(&call(json!({"value": "1"})), &c).is_err());
        let broken = ToolCall {
            id: "t2".into(),
            name: TOOL_NAME.into(),
            input: None,
        };
        assert!(parse_call(&broken, &c).is_err());
        let other = ToolCall {
            id: "t3".into(),
            name: "bash".into(),
            input: Some(json!({})),
        };
        assert!(parse_call(&other, &c).is_err());
    }
}
