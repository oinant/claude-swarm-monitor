//! Parsing des fichiers JSONL de session Claude Code.
//!
//! Chemin: ~/.claude/projects/<url-encoded-path>/sessions/<uuid>.jsonl
//! Chaque ligne = un Record typé.

use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct Record {
    #[serde(rename = "type")]
    pub record_type: RecordType,
    pub uuid: Option<String>,
    #[serde(rename = "parentUuid")]
    pub parent_uuid: Option<String>,
    pub timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub message: Option<Message>,
    #[serde(rename = "parentToolUseId")]
    pub parent_tool_use_id: Option<String>,
    #[serde(rename = "agentId")]
    pub agent_id: Option<String>,
    #[serde(rename = "agentType")]
    pub agent_type: Option<String>,
    #[serde(rename = "teamName")]
    pub team_name: Option<String>,
    #[serde(rename = "isError")]
    pub is_error: Option<bool>,
    pub cost: Option<f64>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum RecordType {
    User,
    Assistant,
    ToolResult,
    System,
    Summary,
    Result,
    #[serde(rename = "file-history-snapshot")]
    FileHistorySnapshot,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct Message {
    pub role: Option<String>,
    pub model: Option<String>,
    pub content: Option<MessageContent>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum MessageContent {
    #[allow(dead_code)]
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Deserialize, Clone)]
pub struct ContentBlock {
    #[serde(rename = "type")]
    pub block_type: String,
    pub text: Option<String>,
    pub id: Option<String>,
    pub name: Option<String>,
    pub input: Option<serde_json::Value>,
    #[allow(dead_code)]
    pub thinking: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Debug, Clone)]
pub enum AgentEvent {
    ToolCall {
        tool_name: String,
        tool_input_summary: String,
    },
    ToolResult {
        is_error: bool,
    },
    TextResponse {
        text: String,
    },
    SpawnSubAgent {
        task_tool_use_id: String,
        prompt_summary: String,
    },
    Completed {
        is_error: bool,
    },
}

pub fn parse_line(line: &str) -> Option<Record> {
    let line = line.trim();
    if line.is_empty() { return None; }
    serde_json::from_str(line).ok()
}

pub fn extract_events(records: &[Record]) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    for record in records {
        match record.record_type {
            RecordType::Assistant => {
                if let Some(msg) = &record.message {
                    if let Some(MessageContent::Blocks(blocks)) = &msg.content {
                        for block in blocks {
                            match block.block_type.as_str() {
                                "tool_use" => {
                                    let tool_name = block.name.clone().unwrap_or_default();
                                    if tool_name == "Task" {
                                        let prompt = block.input.as_ref()
                                            .and_then(|v| v.get("prompt"))
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .chars().take(80).collect::<String>();
                                        events.push(AgentEvent::SpawnSubAgent {
                                            task_tool_use_id: block.id.clone().unwrap_or_default(),
                                            prompt_summary: prompt,
                                        });
                                    } else {
                                        let summary = summarize_tool_input(&tool_name, &block.input);
                                        events.push(AgentEvent::ToolCall {
                                            tool_name,
                                            tool_input_summary: summary,
                                        });
                                    }
                                }
                                "text" => {
                                    if let Some(text) = &block.text {
                                        if !text.trim().is_empty() {
                                            events.push(AgentEvent::TextResponse {
                                                text: text.chars().take(120).collect(),
                                            });
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            RecordType::ToolResult => {
                events.push(AgentEvent::ToolResult { is_error: false });
            }
            RecordType::Result => {
                events.push(AgentEvent::Completed {
                    is_error: record.is_error.unwrap_or(false),
                });
            }
            _ => {}
        }
    }
    events
}

fn summarize_tool_input(tool_name: &str, input: &Option<serde_json::Value>) -> String {
    let Some(input) = input else { return String::new() };
    match tool_name {
        "Bash" => input.get("command").and_then(|v| v.as_str()).unwrap_or("").chars().take(60).collect(),
        "Read" | "Write" | "Edit" | "MultiEdit" => input.get("file_path").and_then(|v| v.as_str()).unwrap_or("").chars().take(60).collect(),
        "Glob" | "Grep" => input.get("pattern").and_then(|v| v.as_str()).unwrap_or("").chars().take(60).collect(),
        _ => input.to_string().chars().take(60).collect(),
    }
}
