#![cfg(feature = "paint")]

use std::cell::RefCell;

use inbrowse_dom::parse_html;
use inbrowse_render::{
    CanvasSurface, CanvasSurfaceSource, CaptureRegion, FrameRasterRequest, RenderResourceCache,
};
use tiny_skia::{Color, Pixmap};

/// Answers every iframe with a solid raster of the requested size and records
/// what was asked for.
struct SolidFrames {
    color: [u8; 4],
    requests: RefCell<Vec<FrameRasterRequest>>,
}

impl SolidFrames {
    fn new(color: [u8; 4]) -> Self {
        Self {
            color,
            requests: RefCell::new(Vec::new()),
        }
    }
}

impl CanvasSurfaceSource for SolidFrames {
    fn surface(&self, _node: inbrowse_dom::tree::NodeId) -> Option<CanvasSurface<'_>> {
        None
    }

    fn frame_raster(&self, request: FrameRasterRequest) -> Option<Pixmap> {
        self.requests.borrow_mut().push(request);
        let mut raster = Pixmap::new(
            request.visible.width as u32,
            request.visible.height as u32,
        )?;
        raster.fill(Color::from_rgba8(
            self.color[0],
            self.color[1],
            self.color[2],
            self.color[3],
        ));
        Some(raster)
    }
}

fn paint(markup: &str, frames: &SolidFrames, viewport: (f32, f32)) -> Pixmap {
    let tree = parse_html(&format!(
        "<style>html,body{{margin:0}}iframe{{position:absolute;border:0}}</style>{markup}"
    ));
    let mut resources = RenderResourceCache::default();
    let mut prepared = inbrowse_render::prepare_dom(&tree, viewport, None, &mut resources).unwrap();
    let scroll = prepared.resolve_scroll_state(&tree, (0.0, 0.0), &Default::default());
    inbrowse_render::paint_prepared_with_scroll_and_surface_color_and_canvas_surfaces(
        &tree,
        &mut prepared,
        &mut resources,
        &scroll,
        [255, 255, 255, 255],
        frames,
    )
    .unwrap()
}

fn pixel(pixmap: &Pixmap, x: u32, y: u32) -> [u8; 4] {
    let pixel = pixmap.pixel(x, y).unwrap();
    [pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()]
}

#[test]
fn iframe_raster_fills_the_content_box_and_nothing_else() {
    let frames = SolidFrames::new([255, 0, 0, 255]);
    let pixmap = paint(
        "<iframe style='left:20px;top:10px;width:60px;height:40px'></iframe>",
        &frames,
        (120.0, 80.0),
    );
    let requests = frames.requests.borrow();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].size, (60, 40));
    assert_eq!(
        requests[0].visible,
        CaptureRegion::new(0.0, 0.0, 60.0, 40.0, 1.0)
    );
    assert!(requests[0].paint_backgrounds);
    assert_eq!(pixel(&pixmap, 20, 10), [255, 0, 0, 255]);
    assert_eq!(pixel(&pixmap, 79, 49), [255, 0, 0, 255]);
    assert_eq!(pixel(&pixmap, 19, 30), [255, 255, 255, 255]);
    assert_eq!(pixel(&pixmap, 80, 30), [255, 255, 255, 255]);
    assert_eq!(pixel(&pixmap, 50, 9), [255, 255, 255, 255]);
    assert_eq!(pixel(&pixmap, 50, 50), [255, 255, 255, 255]);
}

#[test]
fn iframe_raster_excludes_border_and_padding() {
    let frames = SolidFrames::new([0, 255, 0, 255]);
    let pixmap = paint(
        "<iframe style='left:10px;top:10px;width:40px;height:30px;border:5px solid blue;padding:3px'></iframe>",
        &frames,
        (100.0, 80.0),
    );
    let requests = frames.requests.borrow();
    assert_eq!(requests[0].size, (40, 30));
    assert_eq!(pixel(&pixmap, 12, 30), [0, 0, 255, 255]);
    assert_eq!(pixel(&pixmap, 16, 30), [255, 255, 255, 255]);
    assert_eq!(pixel(&pixmap, 18, 30), [0, 255, 0, 255]);
    assert_eq!(pixel(&pixmap, 57, 30), [0, 255, 0, 255]);
    assert_eq!(pixel(&pixmap, 58, 30), [255, 255, 255, 255]);
}

#[test]
fn iframe_raster_requests_only_the_part_inside_the_capture() {
    let frames = SolidFrames::new([255, 0, 0, 255]);
    let pixmap = paint(
        "<iframe style='left:-10px;top:30px;width:60px;height:100px'></iframe>",
        &frames,
        (100.0, 80.0),
    );
    let requests = frames.requests.borrow();
    assert_eq!(requests.len(), 1);
    // The child is laid out at its own size; only its on-screen part is rastered.
    assert_eq!(requests[0].size, (60, 100));
    assert_eq!(
        requests[0].visible,
        CaptureRegion::new(10.0, 0.0, 50.0, 50.0, 1.0)
    );
    assert_eq!(pixel(&pixmap, 0, 30), [255, 0, 0, 255]);
    assert_eq!(pixel(&pixmap, 49, 79), [255, 0, 0, 255]);
    assert_eq!(pixel(&pixmap, 50, 79), [255, 255, 255, 255]);
    assert_eq!(pixel(&pixmap, 20, 29), [255, 255, 255, 255]);
}

#[test]
fn iframe_outside_the_capture_is_never_rasterized() {
    let frames = SolidFrames::new([255, 0, 0, 255]);
    paint(
        "<iframe style='left:0;top:500px;width:60px;height:40px'></iframe>",
        &frames,
        (100.0, 80.0),
    );
    assert!(frames.requests.borrow().is_empty());
}

#[test]
fn print_economy_capture_tells_the_frame_to_skip_backgrounds() {
    let tree = parse_html(
        "<style>html,body{margin:0}iframe{position:absolute;border:0;left:0;top:0;width:20px;height:20px}</style><iframe></iframe>",
    );
    let frames = SolidFrames::new([255, 0, 0, 255]);
    let mut resources = RenderResourceCache::default();
    let mut prepared =
        inbrowse_render::prepare_dom(&tree, (40.0, 40.0), None, &mut resources).unwrap();
    let scroll = prepared.resolve_scroll_state(&tree, (0.0, 0.0), &Default::default());
    inbrowse_render::paint_prepared_region_with_scroll_and_backgrounds_and_canvas_surfaces(
        &tree,
        &mut prepared,
        &mut resources,
        &scroll,
        CaptureRegion::new(0.0, 0.0, 40.0, 40.0, 1.0),
        false,
        [255, 255, 255, 255],
        &frames,
    )
    .unwrap();
    let requests = frames.requests.borrow();
    assert_eq!(requests.len(), 1);
    assert!(!requests[0].paint_backgrounds);
}

#[test]
fn page_without_iframes_asks_for_no_frame_rasters() {
    let frames = SolidFrames::new([255, 0, 0, 255]);
    paint("<div style='width:20px;height:20px'></div>", &frames, (40.0, 40.0));
    assert!(frames.requests.borrow().is_empty());
}
