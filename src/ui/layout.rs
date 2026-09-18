use ratatui::layout::Rect;

/// Percentage/min-size constants transcribed from each btop box's `draw_box`
/// member (`width_p`/`height_p`/`min_width`/`min_height` in `btop_draw.cpp`).
pub struct BoxSpec {
    pub width_p: u32,
    pub height_p: u32,
    pub min_width: u16,
    pub min_height: u16,
}

/// ENGINE ~ cpu box, KV ~ mem, TRAFFIC ~ net, RANKS ~ proc.
///
/// Min widths are tuned so the full four-box grid fits a 55-column phone
/// terminal (btop's own 60-wide floor would leave it showing `toosmall`);
/// rows clip at the frame edge via write_lines rather than hiding data.
pub const ENGINE: BoxSpec = BoxSpec {
    width_p: 100,
    height_p: 32,
    min_width: 55,
    min_height: 8,
};
pub const KV: BoxSpec = BoxSpec {
    width_p: 45,
    height_p: 40,
    min_width: 24,
    min_height: 10,
};
pub const TRAFFIC: BoxSpec = BoxSpec {
    width_p: 45,
    height_p: 28,
    min_width: 24,
    min_height: 6,
};
pub const RANKS: BoxSpec = BoxSpec {
    width_p: 55,
    height_p: 68,
    min_width: 31,
    min_height: 16,
};

/// Which of the four boxes are currently shown (btop's `1`–`4` toggles).
#[derive(Debug, Clone, Copy, Default)]
pub struct Boxes {
    pub engine: bool,
    pub kv: bool,
    pub traffic: bool,
    pub ranks: bool,
}

impl Boxes {
    pub fn all() -> Boxes {
        Boxes {
            engine: true,
            kv: true,
            traffic: true,
            ranks: true,
        }
    }
}

/// The four solved rects. btop draws each box as a COMPLETE independent frame
/// and stacks them with zero gap, so adjacent borders sit on neighbouring
/// rows/columns as a double line — it does NOT merge borders or compute
/// T-junctions (verified against `calcSizes`: cpu `y=1 h=ceil(T*.32)`, mem
/// `y=Cpu.height+1`, net `y=T.height-h+1`, proc `x=T.width-width+1`). We
/// reproduce that exactly: independent, touching frames.
#[derive(Debug, Clone, Copy, Default)]
pub struct Layout {
    pub engine: Rect,
    pub kv: Rect,
    pub traffic: Rect,
    pub ranks: Rect,
}

/// Smallest terminal that can hold the given box set, for `toosmall`.
pub fn min_size(boxes: &Boxes) -> (u16, u16) {
    let left_w = if boxes.kv || boxes.traffic {
        KV.min_width
    } else {
        0
    };
    let right_w = if boxes.ranks { RANKS.min_width } else { 0 };
    let col_w = left_w.saturating_add(right_w);
    let mut min_w = if boxes.engine { ENGINE.min_width } else { 0 };
    min_w = min_w.max(col_w);

    let top_h = if boxes.engine { ENGINE.min_height } else { 0 };
    let left_col_h = if boxes.kv { KV.min_height } else { 0 }
        + if boxes.traffic { TRAFFIC.min_height } else { 0 };
    let right_h = if boxes.ranks { RANKS.min_height } else { 0 };
    let mut min_h = top_h + left_col_h.max(right_h);
    if min_h == 0 {
        min_h = 1;
    }
    (min_w.max(2), min_h.max(2))
}

/// Port of btop `Draw::calcSizes()` in 0-based `Rect` coordinates (btop uses
/// 1-based `x`/`y`; the `-1` is applied per-field). Boxes that are hidden are
/// collapsed to an empty rect; the visible ones tile the area with the same
/// ceil/floor/subtract scheme btop uses so the grid exactly fills the terminal.
pub fn solve(area: Rect, boxes: &Boxes) -> Layout {
    let t_w = area.width;
    let t_h = area.height;

    // --- column split: left (kv|traffic) and right (ranks) are adjacent. btop
    // sets mem/net width = round(T * (Proc::shown ? width_p : 100) / 100): when
    // proc is hidden the left boxes widen to full width. ---
    let left_w = if boxes.kv || boxes.traffic {
        let wp = if boxes.ranks { KV.width_p } else { 100 };
        ((t_w as f64 * wp as f64 / 100.0).round() as u16).max(KV.min_width.min(t_w))
    } else {
        0
    };
    let ranks_w = if boxes.ranks {
        t_w.saturating_sub(left_w)
    } else {
        0
    };

    // --- row split: engine is ceil, kv is floor, traffic absorbs the rest so
    // the left column reaches the terminal bottom exactly. ---
    let engine_h = if boxes.engine {
        ((t_h as f64 * ENGINE.height_p as f64 / 100.0).ceil() as u16)
            .max(ENGINE.min_height.min(t_h))
    } else {
        0
    };
    let below = t_h.saturating_sub(engine_h);
    let kv_h = if boxes.kv {
        if boxes.traffic {
            ((t_h as f64 * KV.height_p as f64 / 100.0).floor() as u16)
                .max(KV.min_height.min(below))
                .min(below)
        } else {
            below
        }
    } else {
        0
    };
    let traffic_h = if boxes.traffic {
        below.saturating_sub(kv_h)
    } else {
        0
    };

    let engine = Rect {
        x: area.x,
        y: area.y,
        width: t_w,
        height: engine_h,
    };

    let ranks_x = area.x + left_w;

    let kv = Rect {
        x: area.x,
        y: area.y + engine_h,
        width: left_w,
        height: kv_h,
    };
    let traffic = Rect {
        x: area.x,
        y: area.y + engine_h + kv_h,
        width: left_w,
        height: traffic_h,
    };
    let ranks = Rect {
        x: ranks_x,
        y: area.y + engine_h,
        width: ranks_w,
        height: t_h.saturating_sub(engine_h),
    };

    Layout {
        engine,
        kv,
        traffic,
        ranks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_btop_calc_sizes_at_120x40() {
        // btop 1-based: cpu(1,1,120,13) mem(1,14,54,16) net(1,30,54,11)
        //              proc(55,14,66,27). 0-based → subtract 1 from x,y.
        let l = solve(Rect::new(0, 0, 120, 40), &Boxes::all());
        assert_eq!(l.engine, Rect::new(0, 0, 120, 13));
        assert_eq!(l.kv, Rect::new(0, 13, 54, 16));
        assert_eq!(l.traffic, Rect::new(0, 29, 54, 11));
        assert_eq!(l.ranks, Rect::new(54, 13, 66, 27));
    }

    #[test]
    fn boxes_touch_no_overlap() {
        // Independent frames: edges abut exactly (double border), never overlap,
        // never leave a gap row/col between neighbours.
        let l = solve(Rect::new(0, 0, 120, 40), &Boxes::all());
        assert_eq!(l.engine.bottom(), l.kv.top(), "engine/kv abut");
        assert_eq!(l.kv.bottom(), l.traffic.top(), "kv/traffic abut");
        assert_eq!(l.traffic.bottom(), 40, "traffic reaches bottom");
        assert_eq!(l.kv.right(), l.ranks.left(), "kv/ranks abut");
        assert_eq!(l.ranks.right(), 120, "ranks reaches right edge");
    }

    #[test]
    fn grid_always_tiles_top_and_left_edges() {
        for h in 24..120 {
            let l = solve(Rect::new(0, 0, 120, h), &Boxes::all());
            assert_eq!(l.traffic.bottom(), h, "bottom at height {h}");
            assert_eq!(l.ranks.right(), 120, "right edge at width 120");
        }
    }

    #[test]
    fn hidden_ranks_widens_left_boxes() {
        // btop: mem/net width = round(T*(Proc::shown ? 45 : 100)/100), so
        // hiding proc widens the left column to full width.
        let boxes = Boxes {
            ranks: false,
            ..Boxes::all()
        };
        let l = solve(Rect::new(0, 0, 120, 40), &boxes);
        assert_eq!(l.kv.width, 120);
    }

    #[test]
    fn no_hidden_box_overflows_the_area() {
        // A collapsed (hidden) box must be an empty rect, not a full-size
        // one positioned past the edge — the fuzz over every size × visible
        // mask found `ranks` at x=36 w=36 on a 36-wide terminal when hidden.
        let boxes = Boxes {
            engine: false,
            kv: false,
            traffic: true,
            ranks: false,
        };
        let l = solve(Rect::new(0, 0, 36, 6), &boxes);
        assert_eq!(l.ranks.area(), 0);
        assert_eq!(l.engine.area(), 0);
        for (name, r) in [("kv", l.kv), ("traffic", l.traffic)] {
            assert!(r.bottom() <= 6 && r.right() <= 36, "{name} {r:?}");
        }
    }

    #[test]
    fn fuzz_every_size_and_visible_mask() {
        // No *visible* box may ever exceed the terminal — a pane drawn past
        // the last row panics in the buffer index (seen live against a real
        // server). Hidden boxes are skipped by the app, so only assert for
        // boxes the mask marks visible.
        for w in 2u16..160 {
            for h in 2u16..80 {
                let area = Rect::new(0, 0, w, h);
                for e in [true, false] {
                    for k in [true, false] {
                        for t in [true, false] {
                            for r in [true, false] {
                                let boxes = Boxes {
                                    engine: e,
                                    kv: k,
                                    traffic: t,
                                    ranks: r,
                                };
                                let (mw, mh) = min_size(&boxes);
                                if w < mw || h < mh {
                                    continue; // toosmall screen renders instead
                                }
                                let l = solve(area, &boxes);
                                for (name, rect, on) in [
                                    ("engine", l.engine, e),
                                    ("kv", l.kv, k),
                                    ("traffic", l.traffic, t),
                                    ("ranks", l.ranks, r),
                                ] {
                                    if !on {
                                        assert_eq!(
                                            rect.area(),
                                            0,
                                            "{name} hidden but has area: {rect:?}"
                                        );
                                        continue;
                                    }
                                    assert!(
                                        rect.bottom() <= h && rect.right() <= w,
                                        "{w}x{h}: {name} {rect:?} exceeds area"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
