use crate::FreeformTool;
use crate::JsonSchema;
use crate::LoadableToolSpec;
use crate::ResponsesApiNamespace;
use crate::ResponsesApiNamespaceTool;
use crate::ResponsesApiTool;
use crate::default_namespace_description;
use codex_protocol::DEFAULT_FUNCTION_NAMESPACE;
use codex_protocol::config_types::WebSearchContextSize;
use codex_protocol::config_types::WebSearchFilters as ConfigWebSearchFilters;
use codex_protocol::config_types::WebSearchUserLocation as ConfigWebSearchUserLocation;
use codex_protocol::config_types::WebSearchUserLocationType;
use serde::Serialize;
use serde_json::Value;
use serde_json::value::RawValue;
use std::sync::Arc;
use tracing::debug;
use tracing::warn;

/// When serialized as JSON, this produces a valid "Tool" in the OpenAI
/// Responses API.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type")]
pub enum ToolSpec {
    #[serde(rename = "function")]
    Function(ResponsesApiTool),
    #[serde(rename = "namespace")]
    Namespace(ResponsesApiNamespace),
    #[serde(rename = "tool_search")]
    ToolSearch {
        execution: String,
        description: String,
        parameters: JsonSchema,
    },
    // TODO: Understand why we get an error on web_search although the API docs
    // say it's supported.
    // https://platform.openai.com/docs/guides/tools-web-search?api-mode=responses#:~:text=%7B%20type%3A%20%22web_search%22%20%7D%2C
    // `external_web_access` distinguishes cached from live-capable search, while
    // `indexed_web_access` restricts live fetches to indexed URLs.
    // https://platform.openai.com/docs/guides/tools-web-search#live-internet-access
    #[serde(rename = "web_search")]
    WebSearch {
        #[serde(skip_serializing_if = "Option::is_none")]
        external_web_access: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        indexed_web_access: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        filters: Option<ResponsesApiWebSearchFilters>,
        #[serde(skip_serializing_if = "Option::is_none")]
        user_location: Option<ResponsesApiWebSearchUserLocation>,
        #[serde(skip_serializing_if = "Option::is_none")]
        search_context_size: Option<WebSearchContextSize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        search_content_types: Option<Vec<String>>,
    },
    #[serde(rename = "custom")]
    Freeform(FreeformTool),
}

impl ToolSpec {
    pub fn name(&self) -> &str {
        match self {
            ToolSpec::Function(tool) => tool.name.as_str(),
            ToolSpec::Namespace(namespace) => namespace.name.as_str(),
            ToolSpec::ToolSearch { .. } => "tool_search",
            ToolSpec::WebSearch { .. } => "web_search",
            ToolSpec::Freeform(tool) => tool.name.as_str(),
        }
    }
}

impl From<LoadableToolSpec> for ToolSpec {
    fn from(value: LoadableToolSpec) -> Self {
        match value {
            LoadableToolSpec::Function(tool) => ToolSpec::Function(tool),
            LoadableToolSpec::Namespace(namespace) => ToolSpec::Namespace(namespace),
        }
    }
}

/// Returns JSON values that are compatible with Function Calling in the
/// Responses API:
/// https://platform.openai.com/docs/guides/function-calling?api-mode=responses
pub fn create_tools_json_for_responses_api(
    tools: &[ToolSpec],
) -> Result<Vec<Value>, serde_json::Error> {
    let mut tools_json = Vec::new();

    for tool in tools {
        let json = serde_json::to_value(tool)?;
        tools_json.push(json);
    }

    Ok(tools_json)
}

pub fn create_tools_json_for_responses_lite(
    tools: &[ToolSpec],
) -> Result<Vec<Value>, serde_json::Error> {
    let mut functions = ResponsesApiNamespace {
        name: DEFAULT_FUNCTION_NAMESPACE.to_string(),
        description: default_namespace_description(DEFAULT_FUNCTION_NAMESPACE),
        tools: Vec::new(),
    };
    let mut functions_index = None;
    let mut tools_json = Vec::new();

    for tool in tools {
        match tool {
            ToolSpec::Function(tool) => {
                functions
                    .tools
                    .push(ResponsesApiNamespaceTool::Function(tool.clone()));
            }
            ToolSpec::Freeform(tool) => {
                functions
                    .tools
                    .push(ResponsesApiNamespaceTool::Custom(tool.clone()));
            }
            ToolSpec::Namespace(namespace) if namespace.name == DEFAULT_FUNCTION_NAMESPACE => {
                if !namespace.description.trim().is_empty() {
                    functions.description = namespace.description.clone();
                }
                functions.tools.extend(namespace.tools.clone());
            }
            tool => {
                tools_json.push(serde_json::to_value(tool)?);
                continue;
            }
        }
        functions_index.get_or_insert(tools_json.len());
    }

    if let Some(functions_index) = functions_index
        && !functions.tools.is_empty()
    {
        tools_json.insert(
            functions_index,
            serde_json::to_value(ToolSpec::Namespace(functions))?,
        );
    }

    Ok(tools_json)
}

/// Returns JSON values that are compatible with Function Calling in Chat
/// Completions APIs. Fork addition: upstream codex removed the chat wire; this
/// mirrors the PR #12234 blueprint so chat-only providers keep tool calling.
pub fn create_tools_json_for_chat_completions(
    tools: &[ToolSpec],
) -> Result<Vec<Value>, serde_json::Error> {
    let mut tools_json = Vec::new();

    for tool in tools {
        match tool {
            ToolSpec::Function(function) => {
                tools_json.push(chat_completions_function_tool_json(function));
            }
            ToolSpec::Freeform(freeform) => {
                // A wrapped function-shaped stand-in would advertise a tool
                // whose calls always fail: freeform handlers only accept
                // `ToolPayload::Custom`, while a chat upstream can only produce
                // `ToolPayload::Function`. Explicitly degrade to omitting the
                // tool instead (chat has no freeform concept).
                warn!(
                    "chat wire cannot carry freeform tool '{}'; omitting it from the tool list",
                    freeform.name
                );
            }
            ToolSpec::Namespace(namespace) => {
                // Chat Completions has no namespace concept: expand the namespace
                // into individually flattened function tools.
                for tool in &namespace.tools {
                    match tool {
                        crate::ResponsesApiNamespaceTool::Function(function) => {
                            tools_json.push(chat_completions_function_tool_json(function));
                        }
                        crate::ResponsesApiNamespaceTool::Custom(freeform) => {
                            warn!(
                                "chat wire cannot carry freeform tool '{}'; omitting it from the tool list",
                                freeform.name
                            );
                        }
                    }
                }
            }
            ToolSpec::ToolSearch { .. } | ToolSpec::WebSearch { .. } => {
                // Chat Completions only accepts function tools; these
                // responses-only tools have no function equivalent.
                debug!("responses-only tool omitted from chat tool list");
            }
        }
    }

    Ok(tools_json)
}

fn chat_completions_function_tool_json(function: &ResponsesApiTool) -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": function.name,
            "description": function.description,
            "parameters": function.parameters,
            "strict": function.strict,
        }
    })
}

/// Returns raw JSON that can be embedded directly in a Responses API request.
pub fn create_tools_raw_json_for_responses_api(
    tools: &[ToolSpec],
) -> Result<Arc<RawValue>, serde_json::Error> {
    serde_json::value::to_raw_value(tools).map(Arc::from)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ResponsesApiWebSearchFilters {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_domains: Option<Vec<String>>,
}

impl From<ConfigWebSearchFilters> for ResponsesApiWebSearchFilters {
    fn from(filters: ConfigWebSearchFilters) -> Self {
        Self {
            allowed_domains: filters.allowed_domains,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ResponsesApiWebSearchUserLocation {
    #[serde(rename = "type")]
    pub r#type: WebSearchUserLocationType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
}

impl From<ConfigWebSearchUserLocation> for ResponsesApiWebSearchUserLocation {
    fn from(user_location: ConfigWebSearchUserLocation) -> Self {
        Self {
            r#type: user_location.r#type,
            country: user_location.country,
            region: user_location.region,
            city: user_location.city,
            timezone: user_location.timezone,
        }
    }
}

/// Returns JSON values that are compatible with tool use in the Anthropic
/// Messages API. Fork addition (goose-blueprint transport): function tools map
/// to `{"name", "description", "input_schema"}` blocks; freeform tools wrap
/// their payload into a single `input` string parameter.
pub fn create_tools_json_for_anthropic(
    tools: &[ToolSpec],
) -> Result<Vec<Value>, serde_json::Error> {
    let mut tools_json = Vec::new();

    for tool in tools {
        match tool {
            ToolSpec::Function(function) => {
                tools_json.push(anthropic_tool_json(function));
            }
            ToolSpec::Freeform(freeform) => {
                // Same explicit degradation as the chat converter: function-shaped
                // stand-ins would always fail at dispatch (freeform handlers only
                // accept `ToolPayload::Custom`).
                warn!(
                    "anthropic wire cannot carry freeform tool '{}'; omitting it from the tool list",
                    freeform.name
                );
            }
            ToolSpec::Namespace(namespace) => {
                // The Messages API has no namespace concept: expand the
                // namespace into individually flattened tools.
                for tool in &namespace.tools {
                    match tool {
                        crate::ResponsesApiNamespaceTool::Function(function) => {
                            tools_json.push(anthropic_tool_json(function));
                        }
                        crate::ResponsesApiNamespaceTool::Custom(freeform) => {
                            warn!(
                                "anthropic wire cannot carry freeform tool '{}'; omitting it from the tool list",
                                freeform.name
                            );
                        }
                    }
                }
            }
            ToolSpec::ToolSearch { .. } | ToolSpec::WebSearch { .. } => {
                // The Messages API accepts function-shaped tools only; these
                // responses-only tools have no function equivalent.
                debug!("responses-only tool omitted from anthropic tool list");
            }
        }
    }

    Ok(tools_json)
}

fn anthropic_tool_json(function: &ResponsesApiTool) -> serde_json::Value {
    serde_json::json!({
        "name": function.name,
        "description": function.description,
        "input_schema": function.parameters,
    })
}

#[cfg(test)]
#[path = "tool_spec_tests.rs"]
mod tests;
