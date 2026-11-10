//! Points, sizes and rectangles. Every unit is a logical pixel; the renderer
//! multiplies by the output's scale.

/// A point in logical pixels, measured from the window's top-left corner.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    /// Distance from the left edge.
    pub x: f32,
    /// Distance from the top edge.
    pub y: f32,
}

impl Point {
    /// A point at `x`, `y`.
    pub const fn new(x: f32, y: f32) -> Point {
        Point { x, y }
    }
}

/// A width and height in logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    /// The width.
    pub width: f32,
    /// The height.
    pub height: f32,
}

impl Size {
    /// A size of `width` by `height`.
    pub const fn new(width: f32, height: f32) -> Size {
        Size { width, height }
    }
}

/// A rectangle in logical pixels: its top-left corner and its size.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    /// The left edge.
    pub x: f32,
    /// The top edge.
    pub y: f32,
    /// The width.
    pub width: f32,
    /// The height.
    pub height: f32,
}

impl Rect {
    /// A rectangle with its top-left corner at `x`, `y`.
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    /// The right edge.
    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    /// The bottom edge.
    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    /// Whether the rectangle covers no area.
    pub fn is_empty(&self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }

    /// Whether `point` lies inside. The left and top edges count as inside,
    /// the right and bottom edges don't, so two rectangles that touch never
    /// both contain a point.
    pub fn contains(&self, point: Point) -> bool {
        point.x >= self.x && point.x < self.right() && point.y >= self.y && point.y < self.bottom()
    }

    /// The smallest rectangle that covers both. An empty rectangle adds
    /// nothing.
    pub fn union(&self, other: Rect) -> Rect {
        if other.is_empty() {
            return *self;
        }
        if self.is_empty() {
            return other;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Rect::new(
            x,
            y,
            self.right().max(other.right()) - x,
            self.bottom().max(other.bottom()) - y,
        )
    }
}
