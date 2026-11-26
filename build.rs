use std::{collections::BTreeMap, env, fmt::Write, fs, path::PathBuf};
use tiny_skia_path as sk;

fn main() {
    println!("cargo:rerun-if-changed=assets/lucide.json");
    let icons: BTreeMap<String, String> =
        serde_json::from_str(include_str!("assets/lucide.json")).expect("Lucide artwork");
    let opts = usvg::Options::default();
    let mut output = String::from(
        "fn icon_path(name: &str, size: i32) -> &'static str {\n    match (name, size) {\n",
    );
    for (name, svg) in icons {
        let tree = usvg::Tree::from_str(&svg, &opts).expect("valid Lucide SVG");
        for size in [14, 15, 19, 20] {
            let path: String = drawings(&tree, size, size, true)
                .into_iter()
                .map(|(_, d)| d)
                .collect();
            assert!(!path.is_empty(), "empty icon {name}");
            writeln!(output, "        ({name:?}, {size}) => {path:?},").unwrap();
        }
    }
    output.push_str("        _ => unreachable!(\"unknown icon {name} at {size}px\"),\n    }\n}\n");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::write(out.join("osd_paths.rs"), output).unwrap();
}

fn drawings(tree: &usvg::Tree, width: i32, height: i32, union: bool) -> Vec<(String, String)> {
    let size = tree.size();
    let w = width as f32;
    let h = height as f32;
    let scale = (w / size.width()).min(h / size.height());
    let viewport = sk::Transform::from_scale(scale, scale).post_translate(
        (w - scale * size.width()) / 2.0,
        (h - scale * size.height()) / 2.0,
    );
    let mut paths = Vec::new();
    collect(tree.root(), viewport, union, &mut paths);
    paths
}

fn collect(
    group: &usvg::Group,
    viewport: sk::Transform,
    union: bool,
    out: &mut Vec<(String, String)>,
) {
    fn rgb(paint: &usvg::Paint) -> String {
        match paint {
            usvg::Paint::Color(c) => format!("{:02X}{:02X}{:02X}", c.red, c.green, c.blue),
            _ => panic!("OSD artwork must use solid colors"),
        }
    }
    for node in group.children() {
        match node {
            usvg::Node::Group(group) => collect(group, viewport, union, out),
            usvg::Node::Path(path) => {
                let transform = path.abs_transform().post_concat(viewport);
                if let Some(fill) = path.fill() {
                    out.push((rgb(fill.paint()), ass_path(path.data(), transform, union)));
                }
                if let Some(stroke) = path.stroke() {
                    let mut outline_style = stroke.to_tiny_skia();
                    if union {
                        // A slightly heavier small-size stroke survives both
                        // fractional desktop scaling and HDR compositing.
                        outline_style.width *= 2.0 / 1.7;
                    }
                    if let Some(outline) = path.data().stroke(&outline_style, 4.0) {
                        out.push((rgb(stroke.paint()), ass_path(&outline, transform, union)));
                    }
                }
            }
            _ => panic!("OSD artwork must use vector paths"),
        }
    }
}

enum Edge {
    Line(sk::Point, sk::Point),
    Curve(sk::Point, sk::Point, sk::Point, sk::Point),
}

impl Edge {
    fn area(&self) -> f32 {
        let cross = |a: sk::Point, b: sk::Point| a.x * b.y - b.x * a.y;
        match *self {
            Self::Line(a, b) => cross(a, b),
            Self::Curve(a, b, c, d) => cross(a, b) + cross(b, c) + cross(c, d),
        }
    }
}

fn ass_path(path: &sk::Path, transform: sk::Transform, union: bool) -> String {
    fn point(p: sk::Point) -> String {
        // ASS \p6 uses 32 drawing units per logical pixel.
        format!(
            "{} {}",
            (p.x * 32.).round() as i32,
            (p.y * 32.).round() as i32
        )
    }
    fn cubic(p1: sk::Point, p2: sk::Point, p3: sk::Point) -> String {
        format!("b {} {} {}", point(p1), point(p2), point(p3))
    }
    let path = path
        .clone()
        .transform(transform)
        .expect("finite SVG geometry");
    let mut contours = Vec::<Vec<Edge>>::new();
    let mut last = sk::Point::zero();
    let mut start = last;
    for part in path.segments() {
        use sk::PathSegment::*;
        let (edge, end) = match part {
            MoveTo(p) => {
                contours.push(Vec::new());
                start = p;
                last = p;
                continue;
            }
            LineTo(p) => (Edge::Line(last, p), p),
            QuadTo(p1, p2) => {
                let ctrl = |p: sk::Point| {
                    sk::Point::from_xy((p.x + 2. * p1.x) / 3., (p.y + 2. * p1.y) / 3.)
                };
                (Edge::Curve(last, ctrl(last), ctrl(p2), p2), p2)
            }
            CubicTo(p1, p2, p3) => (Edge::Curve(last, p1, p2, p3), p3),
            Close => (Edge::Line(last, start), start),
        };
        contours.last_mut().unwrap().push(edge);
        last = end;
    }
    // SVG elements are filled independently. Align their contour winding
    // before joining them so overlapping strokes antialias once in libass.
    let reverse = union
        && contours
            .iter()
            .map(|c| c.iter().map(Edge::area).sum::<f32>())
            .max_by(|a, b| a.abs().total_cmp(&b.abs()))
            .is_some_and(|a| a < 0.);
    let mut out = String::new();
    for contour in &contours {
        let count = contour.len();
        for i in 0..count {
            let edge = &contour[if reverse { count - 1 - i } else { i }];
            let (begin, command) = match (edge, reverse) {
                (Edge::Line(a, b), false) | (Edge::Line(b, a), true) => {
                    (*a, format!("l {}", point(*b)))
                }
                (Edge::Curve(a, b, c, d), false) | (Edge::Curve(d, c, b, a), true) => {
                    (*a, cubic(*b, *c, *d))
                }
            };
            if i == 0 {
                write!(out, "m {} ", point(begin)).unwrap();
            }
            write!(out, "{command} ").unwrap();
        }
    }
    out
}
