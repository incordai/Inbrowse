pub mod accessibility;
pub mod captcha;
pub mod context;
#[cfg(test)]
#[path = "../../test-support/fresh_process.rs"]
pub(crate) mod fresh_process;
mod fork_virtual_url;
mod idle_deadline;
pub mod lifecycle;
pub mod page;
#[cfg(feature = "render")]
pub mod pdf;
pub mod profiles;

pub use context::BrowserContext;
pub use lifecycle::{LifecycleState, WaitUntil};
pub use inbrowse_js::HTML_TO_MARKDOWN_JS;
#[cfg(feature = "render")]
pub use inbrowse_js::{
    validate_capture_region, AnimationSample, AnimationSampleMode, AnimationSampleTime,
    CaptureError, CaptureRegion,
};
pub use page::{url_matches_cdp_pattern, NetworkEvent, Page, PageError};
#[cfg(feature = "render")]
pub use pdf::{RasterPdfError, RasterPdfOptions, RasterPdfPageRange};
// Re-exported so the embeddable `inbrowse-sdk` crate (which depends on inbrowse,
// not inbrowse-js) can surface the interception channel types.
pub use inbrowse_js::ops::{InterceptResolution, InterceptedRequest};
