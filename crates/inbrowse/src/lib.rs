pub mod accessibility;
pub mod captcha;
pub mod context;
mod fork_virtual_url;
#[cfg(test)]
#[path = "../../test-support/fresh_process.rs"]
pub(crate) mod fresh_process;
mod idle_deadline;
pub mod lifecycle;
pub mod page;
pub mod page_json;
#[cfg(feature = "render")]
pub mod pdf;
pub mod profiles;

pub use context::BrowserContext;
pub use inbrowse_js::markdown::html_to_markdown_js;
pub use inbrowse_js::HTML_TO_MARKDOWN_JS;
#[cfg(feature = "render")]
pub use inbrowse_js::{
    validate_capture_region, AnimationSample, AnimationSampleMode, AnimationSampleTime,
    CaptureError, CaptureRegion,
};
pub use lifecycle::{LifecycleState, WaitUntil};
pub use page::{url_matches_cdp_pattern, EvalError, NetworkEvent, Page, PageError};
#[cfg(feature = "render")]
pub use pdf::{RasterPdfError, RasterPdfOptions, RasterPdfPageRange};
// Re-exported so the embeddable `inbrowse-sdk` crate (which depends on inbrowse,
// not inbrowse-js) can surface the interception channel types.
pub use inbrowse_js::ops::{InterceptResolution, InterceptedRequest};
