use kurbo::{BezPath, PathEl, Shape};
use serde::{Deserialize, Serialize};

/// Backend-neutral SVG path command. Arcs and shorthand commands are
/// normalized to quadratic/cubic Béziers by kurbo's SVG parser.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SvgPathCommand {
    MoveTo(f32, f32),
    LineTo(f32, f32),
    QuadTo(f32, f32, f32, f32),
    CubicTo(f32, f32, f32, f32, f32, f32),
    Close,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SvgPathData {
    pub commands: Vec<SvgPathCommand>,
    /// Original path-space bounds `[x, y, width, height]`.
    pub bounds: [f32; 4],
}

impl SvgPathData {
    pub fn parse(data: &str) -> Result<Self, String> {
        let path = BezPath::from_svg(data).map_err(|error| error.to_string())?;
        if path.elements().is_empty() {
            return Err("SVG path is empty".to_string());
        }
        let bounds = path.bounding_box();
        let x = bounds.x0 as f32;
        let y = bounds.y0 as f32;
        let commands = path
            .elements()
            .iter()
            .map(|command| match command {
                PathEl::MoveTo(point) => {
                    SvgPathCommand::MoveTo(point.x as f32 - x, point.y as f32 - y)
                }
                PathEl::LineTo(point) => {
                    SvgPathCommand::LineTo(point.x as f32 - x, point.y as f32 - y)
                }
                PathEl::QuadTo(control, point) => SvgPathCommand::QuadTo(
                    control.x as f32 - x,
                    control.y as f32 - y,
                    point.x as f32 - x,
                    point.y as f32 - y,
                ),
                PathEl::CurveTo(control1, control2, point) => SvgPathCommand::CubicTo(
                    control1.x as f32 - x,
                    control1.y as f32 - y,
                    control2.x as f32 - x,
                    control2.y as f32 - y,
                    point.x as f32 - x,
                    point.y as f32 - y,
                ),
                PathEl::ClosePath => SvgPathCommand::Close,
            })
            .collect();
        Ok(Self {
            commands,
            bounds: [
                x,
                y,
                (bounds.x1 - bounds.x0) as f32,
                (bounds.y1 - bounds.y0) as f32,
            ],
        })
    }

    pub fn from_points(points: &str, close: bool) -> Result<Self, String> {
        let values = points
            .split(|ch: char| ch.is_ascii_whitespace() || ch == ',')
            .filter(|part| !part.is_empty())
            .map(|part| {
                part.parse::<f32>()
                    .map_err(|_| format!("invalid SVG point `{part}`"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if values.len() < 2 || values.len() % 2 != 0 {
            return Err("SVG points must contain coordinate pairs".to_string());
        }
        let mut data = format!("M {} {}", values[0], values[1]);
        for pair in values[2..].chunks_exact(2) {
            data.push_str(&format!(" L {} {}", pair[0], pair[1]));
        }
        if close {
            data.push_str(" Z");
        }
        Self::parse(&data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_relative_shorthand_and_arc_commands() {
        let path = SvgPathData::parse("M10 20h10v10a5 5 0 0 1 5 5z").unwrap();
        assert_eq!(path.bounds[0], 10.0);
        assert_eq!(path.bounds[1], 20.0);
        assert!(path.bounds[2] >= 15.0);
        assert!(
            path.commands
                .iter()
                .any(|command| matches!(command, SvgPathCommand::CubicTo(..)))
        );
        assert!(matches!(path.commands.last(), Some(SvgPathCommand::Close)));
    }

    #[test]
    fn parses_polyline_and_polygon_points() {
        let polyline = SvgPathData::from_points("10,20 30 40", false).unwrap();
        assert_eq!(polyline.bounds, [10.0, 20.0, 20.0, 20.0]);
        assert!(!matches!(
            polyline.commands.last(),
            Some(SvgPathCommand::Close)
        ));
        let polygon = SvgPathData::from_points("0,0 10,0 10,10", true).unwrap();
        assert!(matches!(
            polygon.commands.last(),
            Some(SvgPathCommand::Close)
        ));
    }
}
