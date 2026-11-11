#![cfg(feature = "paint")]

use base64::Engine;
use inbrowse_dom::parse_html;
use inbrowse_render::paint_dom;
use tiny_skia::Pixmap;

const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];

/// A `data:` PNG URL of a 4x4 tile whose left half is red and right half blue.
fn tile_url() -> String {
    let image = image::RgbaImage::from_fn(4, 4, |x, _| {
        image::Rgba(if x < 2 { RED } else { BLUE })
    });
    let mut bytes = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// Paints one 20x12 box (plus `extra` declarations) over a white page.
fn paint(box_css: &str, viewport: (f32, f32)) -> Pixmap {
    let tree = parse_html(&format!(
        "<style>html,body{{margin:0;background:white}}\
         #box{{position:absolute;left:0;top:0;width:20px;height:12px;{box_css}}}</style>\
         <div id=box></div>"
    ));
    paint_dom(&tree, viewport, None).expect("paint")
}

fn pixel(pixmap: &Pixmap, x: u32, y: u32) -> [u8; 4] {
    let pixel = pixmap.pixel(x, y).unwrap();
    [pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()]
}

#[test]
fn small_image_tiles_in_both_directions_by_default() {
    let pixmap = paint(&format!("background:url({})", tile_url()), (40.0, 30.0));
    assert_eq!(pixel(&pixmap, 0, 0), RED);
    assert_eq!(pixel(&pixmap, 1, 3), RED);
    assert_eq!(pixel(&pixmap, 2, 0), BLUE);
    assert_eq!(pixel(&pixmap, 4, 0), RED, "second tile in the row");
    assert_eq!(pixel(&pixmap, 6, 0), BLUE);
    assert_eq!(pixel(&pixmap, 0, 8), RED, "tile in the third row");
    assert_eq!(pixel(&pixmap, 19, 11), BLUE, "last pixel of the box");
    assert_eq!(pixel(&pixmap, 20, 0), WHITE, "nothing outside the box");
    assert_eq!(pixel(&pixmap, 0, 12), WHITE);
}

#[test]
fn repeat_x_and_repeat_y_tile_one_axis() {
    let across = paint(&format!("background:url({}) repeat-x", tile_url()), (40.0, 30.0));
    assert_eq!(pixel(&across, 4, 0), RED);
    assert_eq!(pixel(&across, 16, 3), RED);
    assert_eq!(pixel(&across, 0, 4), WHITE, "no second row");

    let down = paint(&format!("background:url({}) repeat-y", tile_url()), (40.0, 30.0));
    assert_eq!(pixel(&down, 0, 8), RED);
    assert_eq!(pixel(&down, 4, 0), WHITE, "no second column");
}

#[test]
fn no_repeat_paints_one_tile() {
    let pixmap = paint(&format!("background:url({}) no-repeat", tile_url()), (40.0, 30.0));
    assert_eq!(pixel(&pixmap, 0, 0), RED);
    assert_eq!(pixel(&pixmap, 3, 3), BLUE);
    assert_eq!(pixel(&pixmap, 4, 0), WHITE);
    assert_eq!(pixel(&pixmap, 0, 4), WHITE);
}

#[test]
fn background_size_scales_the_tile_before_tiling() {
    let pixmap = paint(
        &format!("background:url({});background-size:8px 8px", tile_url()),
        (40.0, 30.0),
    );
    // Scaling is smooth, so sample away from the red/blue seam of each tile.
    assert_eq!(pixel(&pixmap, 1, 0), RED);
    assert_eq!(pixel(&pixmap, 6, 0), BLUE);
    assert_eq!(pixel(&pixmap, 9, 0), RED, "second scaled tile");
    assert_eq!(pixel(&pixmap, 14, 0), BLUE);
    assert_eq!(pixel(&pixmap, 1, 9), RED, "second scaled row");
}

#[test]
fn background_position_shifts_the_tiling_grid() {
    // Tiles are 8px wide, so centered in 20px the grid starts at x = 6 and
    // continues at -2 and 14.
    let pixmap = paint(
        &format!(
            "background:url({});background-size:8px 8px;background-position:center",
            tile_url()
        ),
        (40.0, 30.0),
    );
    assert_eq!(pixel(&pixmap, 0, 4), RED, "right part of the tile at -2");
    assert_eq!(pixel(&pixmap, 3, 4), BLUE);
    assert_eq!(pixel(&pixmap, 7, 4), RED);
    assert_eq!(pixel(&pixmap, 12, 4), BLUE);
    assert_eq!(pixel(&pixmap, 15, 4), RED);
}

#[test]
fn repeat_space_spreads_whole_tiles_across_the_area() {
    let pixmap = paint(
        &format!(
            "background:url({}) space;background-size:8px 8px",
            tile_url()
        ),
        (40.0, 30.0),
    );
    // Two tiles fit across 20px: 0..8 and 12..20 with a 4px gap. Only one fits
    // down 12px, so the second row stays empty.
    assert_eq!(pixel(&pixmap, 0, 0), RED);
    assert_eq!(pixel(&pixmap, 7, 0), BLUE);
    assert_eq!(pixel(&pixmap, 9, 0), WHITE, "gap between tiles");
    assert_eq!(pixel(&pixmap, 12, 0), RED);
    assert_eq!(pixel(&pixmap, 19, 0), BLUE);
    assert_eq!(pixel(&pixmap, 0, 9), WHITE);
}

#[test]
fn repeat_round_rescales_tiles_to_fill_the_area() {
    let tree = parse_html(&format!(
        "<style>html,body{{margin:0;background:white}}\
         #box{{position:absolute;left:0;top:0;width:18px;height:12px;\
         background:url({}) round;background-size:8px 8px}}</style><div id=box></div>",
        tile_url()
    ));
    let pixmap = paint_dom(&tree, (40.0, 30.0), None).unwrap();
    // 18 / 8 rounds to 2 tiles of 9px across, 12 / 8 rounds to 2 tiles of 6px.
    assert_eq!(pixel(&pixmap, 1, 0), RED);
    assert_eq!(pixel(&pixmap, 7, 0), BLUE);
    assert_eq!(pixel(&pixmap, 10, 0), RED, "second 9px tile");
    assert_eq!(pixel(&pixmap, 16, 0), BLUE);
    assert_eq!(pixel(&pixmap, 1, 8), RED, "second 6px row");
}

#[test]
fn origin_and_clip_boxes_position_and_bound_the_tiling() {
    let content_origin = paint(
        &format!(
            "box-sizing:content-box;padding:4px;background:url({}) no-repeat;\
             background-origin:content-box",
            tile_url()
        ),
        (60.0, 40.0),
    );
    assert_eq!(pixel(&content_origin, 4, 4), RED);
    assert_eq!(pixel(&content_origin, 6, 4), BLUE);
    assert_eq!(pixel(&content_origin, 0, 0), WHITE);

    let content_clip = paint(
        &format!(
            "box-sizing:content-box;padding:4px;background:url({});\
             background-clip:content-box;background-origin:content-box",
            tile_url()
        ),
        (60.0, 40.0),
    );
    assert_eq!(pixel(&content_clip, 4, 4), RED);
    assert_eq!(pixel(&content_clip, 8, 4), RED, "second tile inside the content box");
    assert_eq!(pixel(&content_clip, 2, 2), WHITE, "padding is outside the clip");
    assert_eq!(pixel(&content_clip, 25, 6), WHITE, "right padding is outside the clip");
}

fn svg_url(svg: &str) -> String {
    format!(
        "data:image/svg+xml;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(svg)
    )
}

#[test]
fn svg_with_only_a_ratio_sizes_to_fit_the_positioning_area() {
    let svg = svg_url(
        "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 10 10'>\
         <rect width='10' height='10' fill='red'/></svg>",
    );
    // No intrinsic size, ratio 1: `auto` is `contain`, so a 20x12 box gets one
    // 12x12 square.
    let single = paint(&format!("background:url({svg}) no-repeat"), (40.0, 30.0));
    assert_eq!(pixel(&single, 6, 6), RED);
    assert_eq!(pixel(&single, 11, 11), RED);
    assert_eq!(pixel(&single, 14, 6), WHITE, "right of the square");

    let tiled = paint(&format!("background:url({svg})"), (40.0, 30.0));
    assert_eq!(pixel(&tiled, 14, 6), RED, "the next square tiles in");
    assert_eq!(pixel(&tiled, 6, 0), RED);
}

#[test]
fn svg_without_any_size_fills_the_positioning_area() {
    let svg = svg_url(
        "<svg xmlns='http://www.w3.org/2000/svg' width='100%' height='100%'>\
         <rect width='100%' height='100%' fill='red'/></svg>",
    );
    let pixmap = paint(&format!("background:url({svg}) no-repeat"), (40.0, 30.0));
    assert_eq!(pixel(&pixmap, 1, 1), RED);
    assert_eq!(pixel(&pixmap, 18, 10), RED);
}

#[test]
fn one_pixel_tile_covers_a_large_area() {
    let image = image::RgbaImage::from_pixel(1, 1, image::Rgba(RED));
    let mut bytes = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    let url = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    );
    let tree = parse_html(&format!(
        "<style>html,body{{margin:0}}#box{{width:600px;height:400px;background:url({url})}}</style>\
         <div id=box></div>"
    ));
    let pixmap = paint_dom(&tree, (640.0, 480.0), None).unwrap();
    assert_eq!(pixel(&pixmap, 0, 0), RED);
    assert_eq!(pixel(&pixmap, 599, 399), RED);
    assert_eq!(pixel(&pixmap, 300, 200), RED);
    assert_ne!(pixel(&pixmap, 600, 0), RED);
}

#[test]
fn image_arriving_after_first_layout_tiles_once_cached() {
    let tree = parse_html(
        "<style>html,body{margin:0;background:white}\
         #box{position:absolute;left:0;top:0;width:20px;height:12px;\
         background:url(http://assets.test/tile.png)}</style><div id=box></div>",
    );
    let paint_with = |resources: &mut inbrowse_render::RenderResourceCache| {
        let mut prepared =
            inbrowse_render::prepare_dom(&tree, (40.0, 30.0), None, resources).unwrap();
        let scroll = prepared.resolve_scroll_state(&tree, (0.0, 0.0), &Default::default());
        inbrowse_render::paint_prepared_with_scroll_and_surface_color(
            &tree,
            &mut prepared,
            resources,
            &scroll,
            [255, 255, 255, 255],
        )
        .unwrap()
    };
    let mut resources = inbrowse_render::RenderResourceCache::default();
    resources.set_sync_loading_enabled(false);
    let before = paint_with(&mut resources);
    assert_eq!(pixel(&before, 0, 0), WHITE, "not loaded yet");

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(tile_url().trim_start_matches("data:image/png;base64,"))
        .unwrap();
    resources.seed_shared("http://assets.test/tile.png".to_string(), bytes.into());
    let after = paint_with(&mut resources);
    assert_eq!(pixel(&after, 0, 0), RED);
    assert_eq!(pixel(&after, 4, 0), RED, "tiled once it is known");
    assert_eq!(pixel(&after, 18, 10), BLUE);
}
