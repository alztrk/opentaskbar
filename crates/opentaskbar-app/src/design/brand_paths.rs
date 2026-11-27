//! Exact `Design/Icons/Brand.xaml` AI provider vector logos rendered natively via Direct2D
//! `ID2D1PathGeometry` (zero external SVG crate dependencies, crisp at any DPI).

use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_F, D2D1_BEZIER_SEGMENT, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED,
    D2D1_FIGURE_END_CLOSED, D2D1_FILL_MODE_ALTERNATE, D2D1_FILL_MODE_WINDING,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL, D2D1_QUADRATIC_BEZIER_SEGMENT,
    D2D1_SWEEP_DIRECTION_CLOCKWISE, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE, ID2D1DeviceContext,
    ID2D1Factory1, ID2D1PathGeometry,
};
use windows::core::Result;
use windows_numerics::{Matrix3x2, Vector2};

use super::Rgb;

/// Exact `StreamGeometry` path data from `Design/Icons/Brand.xaml`.
pub fn brand_path_data(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        "aiusage_claude" => Some(
            "F1 m4.7144 15.9555 4.7174-2.6471.079-.2307-.079-.1275h-.2307l-.7893-.0486-2.6956-.0729-2.3375-.0971-2.2646-.1214-.5707-.1215-.5343-.7042.0546-.3522.4797-.3218.686.0608 1.5179.1032 2.2767.1578 1.6514.0972 2.4468.255h.3886l.0546-.1579-.1336-.0971-.1032-.0972L6.973 9.8356l-2.55-1.6879-1.3356-.9714-.7225-.4918-.3643-.4614-.1578-1.0078.6557-.7225.8803.0607.2246.0607.8925.686 1.9064 1.4754 2.4893 1.8336.3643.3035.1457-.1032.0182-.0728-.164-.2733-1.3539-2.4467-1.445-2.4893-.6435-1.032-.17-.6194c-.0607-.255-.1032-.4674-.1032-.7285L6.287.1335 6.6997 0l.9957.1336.419.3642.6192 1.4147 1.0018 2.2282 1.5543 3.0296.4553.8985.2429.8318.091.255h.1579v-.1457l.1275-1.706.2368-2.0947.2307-2.6957.0789-.7589.3764-.9107.7468-.4918.5828.2793.4797.686-.0668.4433-.2853 1.8517-.5586 2.9021-.3643 1.9429h.2125l.2429-.2429.9835-1.3053 1.6514-2.0643.7286-.8196.85-.9046.5464-.4311h1.0321l.759 1.1293-.34 1.1657-1.0625 1.3478-.8804 1.1414-1.2628 1.7-.7893 1.36.0729.1093.1882-.0183 2.8535-.607 1.5421-.2794 1.8396-.3157.8318.3886.091.3946-.3278.8075-1.967.4857-2.3072.4614-3.4364.8136-.0425.0304.0486.0607 1.5482.1457.6618.0364h1.621l3.0175.2247.7892.522.4736.6376-.079.4857-1.2142.6193-1.6393-.3886-3.825-.9107-1.3113-.3279h-.1822v.1093l1.0929 1.0686 2.0035 1.8092 2.5075 2.3314.1275.5768-.3218.4554-.34-.0486-2.2039-1.6575-.85-.7468-1.9246-1.621h-.1275v.17l.4432.6496 2.3436 3.5214.1214 1.0807-.17.3521-.6071.2125-.6679-.1214-1.3721-1.9246L14.38 17.959l-1.1414-1.9428-.1397.079-.674 7.2552-.3156.3703-.7286.2793-.6071-.4614-.3218-.7468.3218-1.4753.3886-1.9246.3157-1.53.2853-1.9004.17-.6314-.0121-.0425-.1397.0182-1.4328 1.9672-2.1796 2.9446-1.7243 1.8456-.4128.164-.7164-.3704.0667-.6618.4008-.5889 2.386-3.0357 1.4389-1.882.929-1.0868-.0062-.1579h-.0546l-6.3385 4.1164-1.1293.1457-.4857-.4554.0608-.7467.2307-.2429 1.9064-1.3114Z",
        ),
        "aiusage_codex" => Some(
            "F0 M8.086.457a 6.105 6.105 0 0 1 3.046 -.415c1.333.153 2.521.72 3.564 1.7a .117 .117 0 0 0 .107 .029c1.408-.346 2.762-.224 4.061.366l.063.03.154.076c1.357.703 2.33 1.77 2.918 3.198.278.679.418 1.388.421 2.126a 5.655 5.655 0 0 1 -.18 1.631 .167 .167 0 0 0 .04 .155 5.982 5.982 0 0 1 1.578 2.891c.385 1.901-.01 3.615-1.183 5.14l-.182.22a 6.063 6.063 0 0 1 -2.934 1.851 .162 .162 0 0 0 -.108 .102c-.255.736-.511 1.364-.987 1.992-1.199 1.582-2.962 2.462-4.948 2.451-1.583-.008-2.986-.587-4.21-1.736a .145 .145 0 0 0 -.14 -.032c-.518.167-1.04.191-1.604.185a 5.924 5.924 0 0 1 -2.595 -.622 6.058 6.058 0 0 1 -2.146 -1.781c-.203-.269-.404-.522-.551-.821a 7.74 7.74 0 0 1 -.495 -1.283 6.11 6.11 0 0 1 -.017 -3.064 .166 .166 0 0 0 .008 -.074 .115 .115 0 0 0 -.037 -.064 5.958 5.958 0 0 1 -1.38 -2.202 5.196 5.196 0 0 1 -.333 -1.589 6.915 6.915 0 0 1 .188 -2.132c.45-1.484 1.309-2.648 2.577-3.493.282-.188.55-.334.802-.438.286-.12.573-.22.861-.304a .129 .129 0 0 0 .087 -.087A 6.016 6.016 0 0 1 5.635 2.31C6.315 1.464 7.132.846 8.086.457zm-.804 7.85a .848 .848 0 0 0 -1.473 .842l1.694 2.965-1.688 2.848a .849 .849 0 0 0 1.46 .864l1.94-3.272a .849 .849 0 0 0 .007 -.854l-1.94-3.393zm5.446 6.24a .849 .849 0 0 0 0 1.695h4.848a .849 .849 0 0 0 0 -1.696h-4.848z",
        ),
        "aiusage_opencode" => Some("F0 M16 6H8v12h8V6zm4 16H4V2h16v20z"),
        "aiusage_copilot" => Some(
            "F1 M23.922 16.997C23.061 18.492 18.063 22.02 12 22.02 5.937 22.02.939 18.492.078 16.997A.641.641 0 0 1 0 16.741v-2.869a.883.883 0 0 1 .053-.22c.372-.935 1.347-2.292 2.605-2.656.167-.429.414-1.055.644-1.517a10.098 10.098 0 0 1-.052-1.086c0-1.331.282-2.499 1.132-3.368.397-.406.89-.717 1.474-.952C7.255 2.937 9.248 1.98 11.978 1.98c2.731 0 4.767.957 6.166 2.093.584.235 1.077.546 1.474.952.85.869 1.132 2.037 1.132 3.368 0 .368-.014.733-.052 1.086.23.462.477 1.088.644 1.517 1.258.364 2.233 1.721 2.605 2.656a.841.841 0 0 1 .053.22v2.869a.641.641 0 0 1-.078.256Zm-11.75-5.992h-.344a4.359 4.359 0 0 1-.355.508c-.77.947-1.918 1.492-3.508 1.492-1.725 0-2.989-.359-3.782-1.259a2.137 2.137 0 0 1-.085-.104L4 11.746v6.585c1.435.779 4.514 2.179 8 2.179 3.486 0 6.565-1.4 8-2.179v-6.585l-.098-.104s-.033.045-.085.104c-.793.9-2.057 1.259-3.782 1.259-1.59 0-2.738-.545-3.508-1.492a4.359 4.359 0 0 1-.355-.508Zm2.328 3.25c.549 0 1 .451 1 1v2c0 .549-.451 1-1 1-.549 0-1-.451-1-1v-2c0-.549.451-1 1-1Zm-5 0c.549 0 1 .451 1 1v2c0 .549-.451 1-1 1-.549 0-1-.451-1-1v-2c0-.549.451-1 1-1Zm3.313-6.185c.136 1.057.403 1.913.878 2.497.442.544 1.134.938 2.344.938 1.573 0 2.292-.337 2.657-.751.384-.435.558-1.15.558-2.361 0-1.14-.243-1.847-.705-2.319-.477-.488-1.319-.862-2.824-1.025-1.487-.161-2.192.138-2.533.529-.269.307-.437.808-.438 1.578v.021c0 .265.021.562.063.893Zm-1.626 0c.042-.331.063-.628.063-.894v-.02c-.001-.77-.169-1.271-.438-1.578-.341-.391-1.046-.69-2.533-.529-1.505.163-2.347.537-2.824 1.025-.462.472-.705 1.179-.705 2.319 0 1.211.175 1.926.558 2.361.365.414 1.084.751 2.657.751 1.21 0 1.902-.394 2.344-.938.475-.584.742-1.44.878-2.497Z",
        ),
        "aiusage_deepseek" => Some(
            "F1 M23.748 4.651c-.254-.124-.364.113-.512.233-.051.04-.094.09-.137.137-.372.397-.806.657-1.373.626-.829-.046-1.537.214-2.163.848-.133-.782-.575-1.248-1.247-1.548-.352-.155-.708-.311-.955-.65-.172-.24-.219-.509-.305-.774-.055-.16-.11-.323-.293-.35-.2-.031-.278.136-.356.276-.313.572-.434 1.202-.422 1.84.027 1.436.633 2.58 1.838 3.393.137.094.172.187.129.323-.082.28-.18.553-.266.833-.055.179-.137.218-.328.14a5.5 5.5 0 0 1-1.737-1.179c-.857-.828-1.631-1.743-2.597-2.46a12 12 0 0 0-.689-.47c-.985-.957.13-1.743.387-1.836.27-.098.094-.433-.778-.428-.872.003-1.67.295-2.687.685a3 3 0 0 1-.465.136 9.6 9.6 0 0 0-2.883-.101c-1.885.21-3.39 1.1-4.497 2.622C.082 8.776-.231 10.854.152 13.02c.403 2.284 1.568 4.175 3.36 5.653 1.857 1.533 3.997 2.284 6.438 2.14 1.482-.085 3.132-.284 4.994-1.86.47.234.962.328 1.78.398.629.058 1.235-.031 1.705-.129.735-.155.684-.836.418-.961-2.155-1.004-1.682-.595-2.112-.926 1.095-1.295 2.768-3.598 3.284-6.733.05-.346.115-.834.108-1.114-.004-.171.035-.238.23-.257a4.2 4.2 0 0 0 1.545-.475c1.397-.763 1.96-2.016 2.093-3.517.02-.23-.004-.467-.247-.588M11.58 18.168c-2.088-1.642-3.101-2.183-3.52-2.16-.39.024-.32.472-.234.763.09.288.207.487.371.74.114.167.192.416-.113.603-.673.416-1.842-.14-1.897-.168-1.361-.801-2.5-1.86-3.301-3.306-.775-1.393-1.225-2.888-1.299-4.482-.02-.385.094-.522.477-.592a4.7 4.7 0 0 1 1.53-.038c2.131.311 3.946 1.264 5.467 2.774.868.86 1.525 1.887 2.202 2.89.72 1.066 1.494 2.082 2.48 2.915.348.291.626.513.892.677-.802.09-2.14.109-3.055-.615zm1.001-6.44a.306.306 0 0 1 .415-.287.3.3 0 0 1 .113.074.3.3 0 0 1 .086.214c0 .17-.136.307-.308.307a.303.303 0 0 1-.306-.307m3.11 1.596c-.2.081-.4.151-.591.16a1.25 1.25 0 0 1-.798-.254c-.274-.23-.47-.358-.551-.758a1.7 1.7 0 0 1 .015-.588c.07-.327-.007-.537-.238-.727-.188-.156-.426-.199-.689-.199a.6.6 0 0 1-.254-.078.253.253 0 0 1-.114-.358 1 1 0 0 1 .192-.21c.356-.202.767-.136 1.146.016.352.144.618.408 1.001.782.392.451.462.576.685.915.176.264.336.536.446.848.066.194-.02.353-.25.45",
        ),
        "aiusage_openrouter" => Some(
            "F1 M16.778 1.844v1.919q-.569-.026-1.138-.032-.708-.008-1.415.037c-1.93.126-4.023.728-6.149 2.237-2.911 2.066-2.731 1.95-4.14 2.75-.396.223-1.342.574-2.185.798-.841.225-1.753.333-1.751.333v4.229s.768.108 1.61.333c.842.224 1.789.575 2.185.799 1.41.798 1.228.683 4.14 2.75 2.126 1.509 4.22 2.11 6.148 2.236.88.058 1.716.041 2.555.005v1.918l7.222-4.168-7.222-4.17v2.176c-.86.038-1.611.065-2.278.021-1.364-.09-2.417-.357-3.979-1.465-2.244-1.593-2.866-2.027-3.68-2.508.889-.518 1.449-.906 3.822-2.59 1.56-1.109 2.614-1.377 3.978-1.466.667-.044 1.418-.017 2.278.02v2.176L24 6.014Z",
        ),
        "aiusage_cursor" => Some(
            "F1 M11.503 .131 1.891 5.678a .84 .84 0 0 0 -.42 .726v11.188c 0 .3 .162 .575 .42 .724l9.609 5.55a 1 1 0 0 0 .998 0l9.61 -5.55a .84 .84 0 0 0 .42 -.724V6.404a .84 .84 0 0 0 -.42 -.726L12.497 .131a 1.01 1.01 0 0 0 -.996 0M2.657 6.338h18.55c .263 0 .43 .287 .297 .515L12.23 22.918c -.062 .107 -.229 .064 -.229 -.06V12.335a .59 .59 0 0 0 -.295 -.51l-9.11 -5.257c -.109 -.063 -.064 -.23 .061 -.23",
        ),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy)]
pub enum PathOp {
    MoveTo(f32, f32),
    LineTo(f32, f32),
    CubicTo(f32, f32, f32, f32, f32, f32),
    QuadTo(f32, f32, f32, f32),
    ArcTo {
        rx: f32,
        ry: f32,
        rot: f32,
        large: bool,
        sweep: bool,
        x: f32,
        y: f32,
    },
    Close,
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            bytes: s.as_bytes(),
            pos: 0,
        }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() {
            match self.bytes[self.pos] {
                b' ' | b'\t' | b'\r' | b'\n' | b',' => self.pos += 1,
                _ => break,
            }
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_ws();
        self.bytes.get(self.pos).copied()
    }

    fn has_number(&mut self) -> bool {
        matches!(self.peek(), Some(b'0'..=b'9' | b'.' | b'-' | b'+'))
    }

    fn next_f32(&mut self) -> Option<f32> {
        self.skip_ws();
        let start = self.pos;
        if self.pos < self.bytes.len() && matches!(self.bytes[self.pos], b'-' | b'+') {
            self.pos += 1;
        }
        let mut seen_dot = false;
        while self.pos < self.bytes.len() {
            match self.bytes[self.pos] {
                b'0'..=b'9' => self.pos += 1,
                b'.' if !seen_dot => {
                    seen_dot = true;
                    self.pos += 1;
                }
                _ => break,
            }
        }
        if self.pos < self.bytes.len() && matches!(self.bytes[self.pos], b'e' | b'E') {
            self.pos += 1;
            if self.pos < self.bytes.len() && matches!(self.bytes[self.pos], b'-' | b'+') {
                self.pos += 1;
            }
            while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
        }
        if self.pos == start {
            return None;
        }
        std::str::from_utf8(&self.bytes[start..self.pos])
            .ok()?
            .parse::<f32>()
            .ok()
    }

    fn next_flag(&mut self) -> Option<bool> {
        self.skip_ws();
        let b = *self.bytes.get(self.pos)?;
        if b == b'0' || b == b'1' {
            self.pos += 1;
            Some(b == b'1')
        } else {
            None
        }
    }
}

pub fn parse_stream_geometry(data: &str) -> (bool, Vec<PathOp>, usize) {
    let mut p = Parser::new(data);
    let mut winding = true;
    if p.peek() == Some(b'F') {
        p.pos += 1;
        if let Some(flag) = p.next_flag() {
            winding = flag;
        }
    }
    let mut ops = Vec::with_capacity(64);
    let (mut cx, mut cy) = (0.0f32, 0.0f32);
    let (mut sx, mut sy) = (0.0f32, 0.0f32);
    let (mut last_cp_x, mut last_cp_y) = (0.0f32, 0.0f32);
    let mut last_cmd = b' ';

    while let Some(b) = p.peek() {
        let cmd = if b.is_ascii_alphabetic() {
            p.pos += 1;
            b
        } else {
            match last_cmd {
                b'M' => b'L',
                b'm' => b'l',
                other => other,
            }
        };
        match cmd {
            b'M' | b'm' => {
                let (Some(x), Some(y)) = (p.next_f32(), p.next_f32()) else {
                    break;
                };
                if cmd == b'm' {
                    cx += x;
                    cy += y;
                } else {
                    cx = x;
                    cy = y;
                }
                sx = cx;
                sy = cy;
                ops.push(PathOp::MoveTo(cx, cy));
                last_cmd = cmd;
            }
            b'L' | b'l' => {
                let (Some(x), Some(y)) = (p.next_f32(), p.next_f32()) else {
                    break;
                };
                if cmd == b'l' {
                    cx += x;
                    cy += y;
                } else {
                    cx = x;
                    cy = y;
                }
                ops.push(PathOp::LineTo(cx, cy));
                last_cmd = cmd;
            }
            b'H' | b'h' => {
                let Some(x) = p.next_f32() else { break };
                cx = if cmd == b'h' { cx + x } else { x };
                ops.push(PathOp::LineTo(cx, cy));
                last_cmd = cmd;
            }
            b'V' | b'v' => {
                let Some(y) = p.next_f32() else { break };
                cy = if cmd == b'v' { cy + y } else { y };
                ops.push(PathOp::LineTo(cx, cy));
                last_cmd = cmd;
            }
            b'C' | b'c' => {
                let (Some(x1), Some(y1), Some(x2), Some(y2), Some(x), Some(y)) = (
                    p.next_f32(),
                    p.next_f32(),
                    p.next_f32(),
                    p.next_f32(),
                    p.next_f32(),
                    p.next_f32(),
                ) else {
                    break;
                };
                let (ax1, ay1, ax2, ay2, ax, ay) = if cmd == b'c' {
                    (cx + x1, cy + y1, cx + x2, cy + y2, cx + x, cy + y)
                } else {
                    (x1, y1, x2, y2, x, y)
                };
                ops.push(PathOp::CubicTo(ax1, ay1, ax2, ay2, ax, ay));
                last_cp_x = ax2;
                last_cp_y = ay2;
                cx = ax;
                cy = ay;
                last_cmd = cmd;
            }
            b'S' | b's' => {
                let (Some(x2), Some(y2), Some(x), Some(y)) =
                    (p.next_f32(), p.next_f32(), p.next_f32(), p.next_f32())
                else {
                    break;
                };
                let (ax1, ay1) = if matches!(last_cmd, b'C' | b'c' | b'S' | b's') {
                    (2.0 * cx - last_cp_x, 2.0 * cy - last_cp_y)
                } else {
                    (cx, cy)
                };
                let (ax2, ay2, ax, ay) = if cmd == b's' {
                    (cx + x2, cy + y2, cx + x, cy + y)
                } else {
                    (x2, y2, x, y)
                };
                ops.push(PathOp::CubicTo(ax1, ay1, ax2, ay2, ax, ay));
                last_cp_x = ax2;
                last_cp_y = ay2;
                cx = ax;
                cy = ay;
                last_cmd = cmd;
            }
            b'Q' | b'q' => {
                let (Some(x1), Some(y1), Some(x), Some(y)) =
                    (p.next_f32(), p.next_f32(), p.next_f32(), p.next_f32())
                else {
                    break;
                };
                let (ax1, ay1, ax, ay) = if cmd == b'q' {
                    (cx + x1, cy + y1, cx + x, cy + y)
                } else {
                    (x1, y1, x, y)
                };
                ops.push(PathOp::QuadTo(ax1, ay1, ax, ay));
                last_cp_x = ax1;
                last_cp_y = ay1;
                cx = ax;
                cy = ay;
                last_cmd = cmd;
            }
            b'A' | b'a' => {
                let (Some(rx), Some(ry), Some(rot), Some(large), Some(sweep), Some(x), Some(y)) = (
                    p.next_f32(),
                    p.next_f32(),
                    p.next_f32(),
                    p.next_flag(),
                    p.next_flag(),
                    p.next_f32(),
                    p.next_f32(),
                ) else {
                    break;
                };
                let (ax, ay) = if cmd == b'a' {
                    (cx + x, cy + y)
                } else {
                    (x, y)
                };
                ops.push(PathOp::ArcTo {
                    rx: rx.abs(),
                    ry: ry.abs(),
                    rot,
                    large,
                    sweep,
                    x: ax,
                    y: ay,
                });
                cx = ax;
                cy = ay;
                last_cmd = cmd;
            }
            b'Z' | b'z' => {
                ops.push(PathOp::Close);
                cx = sx;
                cy = sy;
                last_cmd = cmd;
            }
            _ => {
                if !p.has_number() {
                    p.pos += 1;
                } else {
                    break;
                }
            }
        }
    }
    (winding, ops, p.pos)
}

unsafe fn build_path_geometry(factory: &ID2D1Factory1, data: &str) -> Result<ID2D1PathGeometry> {
    let (winding, ops, consumed) = parse_stream_geometry(data);
    // Never silently draw a truncated icon: user-supplied `WidgetIcon::PathData`
    // goes through this path, and partial parsing used to draw half a shape.
    if consumed != data.len() {
        return Err(windows::Win32::Foundation::E_INVALIDARG.into());
    }
    unsafe {
        let geom = factory.CreatePathGeometry()?;
        let sink = geom.Open()?;
        sink.SetFillMode(if winding {
            D2D1_FILL_MODE_WINDING
        } else {
            D2D1_FILL_MODE_ALTERNATE
        });
        let mut figure_open = false;
        for op in ops {
            match op {
                PathOp::MoveTo(x, y) => {
                    if figure_open {
                        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                    }
                    sink.BeginFigure(Vector2 { X: x, Y: y }, D2D1_FIGURE_BEGIN_FILLED);
                    figure_open = true;
                }
                PathOp::LineTo(x, y) => {
                    if figure_open {
                        sink.AddLine(Vector2 { X: x, Y: y });
                    }
                }
                PathOp::CubicTo(x1, y1, x2, y2, x, y) => {
                    if figure_open {
                        sink.AddBezier(&D2D1_BEZIER_SEGMENT {
                            point1: Vector2 { X: x1, Y: y1 },
                            point2: Vector2 { X: x2, Y: y2 },
                            point3: Vector2 { X: x, Y: y },
                        });
                    }
                }
                PathOp::QuadTo(x1, y1, x, y) => {
                    if figure_open {
                        sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT {
                            point1: Vector2 { X: x1, Y: y1 },
                            point2: Vector2 { X: x, Y: y },
                        });
                    }
                }
                PathOp::ArcTo {
                    rx,
                    ry,
                    rot,
                    large,
                    sweep,
                    x,
                    y,
                } => {
                    if figure_open {
                        sink.AddArc(&D2D1_ARC_SEGMENT {
                            point: Vector2 { X: x, Y: y },
                            size: D2D_SIZE_F {
                                width: rx,
                                height: ry,
                            },
                            rotationAngle: rot,
                            sweepDirection: if sweep {
                                D2D1_SWEEP_DIRECTION_CLOCKWISE
                            } else {
                                D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE
                            },
                            arcSize: if large {
                                D2D1_ARC_SIZE_LARGE
                            } else {
                                D2D1_ARC_SIZE_SMALL
                            },
                        });
                    }
                }
                PathOp::Close => {
                    if figure_open {
                        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                        figure_open = false;
                    }
                }
            }
        }
        if figure_open {
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        }
        sink.Close()?;
        Ok(geom.into())
    }
}

/// Draws the exact `Brand.xaml` vector silhouette uniformly centered inside `dest`.
pub unsafe fn draw_brand_logo(
    factory: &ID2D1Factory1,
    dc: &ID2D1DeviceContext,
    provider_id: &str,
    dest: D2D_RECT_F,
    color: Rgb,
    alpha: f32,
) -> Result<()> {
    let Some(data) = brand_path_data(provider_id) else {
        return Ok(());
    };
    unsafe { draw_path_data_logo(factory, dc, data, dest, color, alpha) }
}

/// Draws arbitrary `StreamGeometry` path data (Lucide-style `M… L… C… Z`) into `dest`.
/// Used by built-in brand logos and by user-supplied widget icons; no SVG crate needed.
pub unsafe fn draw_path_data_logo(
    factory: &ID2D1Factory1,
    dc: &ID2D1DeviceContext,
    data: &str,
    dest: D2D_RECT_F,
    color: Rgb,
    alpha: f32,
) -> Result<()> {
    unsafe {
        let geom = build_path_geometry(factory, data)?;
        let bounds = geom.GetBounds(None)?;
        let bw = (bounds.right - bounds.left).max(0.01);
        let bh = (bounds.bottom - bounds.top).max(0.01);
        let dw = (dest.right - dest.left).max(1.0);
        let dh = (dest.bottom - dest.top).max(1.0);
        let scale = (dw / bw).min(dh / bh);
        let tx = dest.left + (dw - bw * scale) * 0.5 - bounds.left * scale;
        let ty = dest.top + (dh - bh * scale) * 0.5 - bounds.top * scale;

        let mut prev = Matrix3x2::default();
        dc.GetTransform(&mut prev);
        let local = Matrix3x2 {
            M11: scale,
            M12: 0.0,
            M21: 0.0,
            M22: scale,
            M31: tx,
            M32: ty,
        };
        // Compose local transform before existing DPI transform (`local * prev`).
        let combined = Matrix3x2 {
            M11: local.M11 * prev.M11 + local.M12 * prev.M21,
            M12: local.M11 * prev.M12 + local.M12 * prev.M22,
            M21: local.M21 * prev.M11 + local.M22 * prev.M21,
            M22: local.M21 * prev.M12 + local.M22 * prev.M22,
            M31: local.M31 * prev.M11 + local.M32 * prev.M21 + prev.M31,
            M32: local.M31 * prev.M12 + local.M32 * prev.M22 + prev.M32,
        };
        dc.SetTransform(&combined);
        // Restore on every exit path, including the `?` on brush creation below.
        let _restore = super::d2d::TransformGuard::new(&prev, |m: &Matrix3x2| dc.SetTransform(m));
        let brush = dc.CreateSolidColorBrush(
            &D2D1_COLOR_F {
                r: f32::from(color.0) / 255.0,
                g: f32::from(color.1) / 255.0,
                b: f32::from(color.2) / 255.0,
                a: alpha.clamp(0.0, 1.0),
            },
            None,
        )?;
        dc.FillGeometry(&geom, &brush, None);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{brand_path_data, parse_stream_geometry};

    #[test]
    fn all_seven_reference_brand_logos_parse_cleanly() {
        let ids = [
            "aiusage_claude",
            "aiusage_codex",
            "aiusage_opencode",
            "aiusage_copilot",
            "aiusage_deepseek",
            "aiusage_openrouter",
            "aiusage_cursor",
        ];
        for id in ids {
            let raw = brand_path_data(id).expect("missing brand path");
            let (_, ops, consumed) = parse_stream_geometry(raw);
            assert_eq!(
                consumed,
                raw.len(),
                "parser stopped early for {id} at {consumed}/{}",
                raw.len()
            );
            assert!(
                ops.len() >= 5,
                "expected non-trivial path ops for {id}, got {}",
                ops.len()
            );
        }
    }

    #[test]
    fn malformed_path_data_is_rejected_instead_of_drawn_partially() {
        // `build_path_geometry` now refuses input the parser could not fully
        // consume. These all stop early with a partially built op list.
        for bad in ["M0 0 X10 10", "M0 0 L10 10 Z Q ? 1"] {
            let (_, _, consumed) = parse_stream_geometry(bad);
            assert!(
                consumed != bad.len(),
                "{bad:?} was fully consumed; expected malformed input"
            );
        }
        // Valid input is still fully consumed.
        let (_, _, consumed) = parse_stream_geometry("M0 0 L10 10 Z");
        assert_eq!(consumed, "M0 0 L10 10 Z".len());
    }
}
