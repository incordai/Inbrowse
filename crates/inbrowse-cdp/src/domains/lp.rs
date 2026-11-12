use inbrowse::HTML_TO_MARKDOWN_JS;
use serde_json::{json, Value};

use crate::dispatch::CdpContext;

pub async fn handle(
    method: &str,
    params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    match method {
        // Boilerplate (navigation, header, footer, sidebars) is left out
        // unless `boilerplate` is "keep".
        "getMarkdown" => {
            let drop_boilerplate = match params.get("boilerplate").and_then(Value::as_str) {
                None | Some("drop") => true,
                Some("keep") => false,
                Some(other) => {
                    return Err(format!(
                        "boilerplate must be \"keep\" or \"drop\", not \"{other}\""
                    ))
                }
            };
            let page = ctx.get_session_page_mut(session_id).ok_or("No page")?;
            let result = if drop_boilerplate {
                page.evaluate(HTML_TO_MARKDOWN_JS)?
            } else {
                page.evaluate(&inbrowse::html_to_markdown_js(None, false))?
            };
            let markdown = result
                .as_str()
                .ok_or("markdown conversion did not return text")?;
            Ok(json!({ "markdown": markdown }))
        }
        _ => Err(format!("Unknown LP method: {}", method)),
    }
}
