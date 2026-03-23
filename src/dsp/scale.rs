#[derive(Debug, Clone, Copy)]
pub enum ScaleKind {
    Chromatic,
    Major,
    Minor,
}

#[derive(Debug, Clone)]
pub struct ScaleMapper {
    root_pc: i32,
    kind: ScaleKind,
    enabled_pc: [bool; 12],
}

impl ScaleMapper {
    pub fn new(root_pc: i32, kind: ScaleKind) -> Self {
        let mut mapper = Self {
            root_pc: root_pc.rem_euclid(12),
            kind,
            enabled_pc: [false; 12],
        };
        mapper.rebuild_mask();
        mapper
    }

    pub fn set_scale(&mut self, root_pc: i32, kind: ScaleKind) {
        self.root_pc = root_pc.rem_euclid(12);
        self.kind = kind;
        self.rebuild_mask();
    }

    pub fn root_pc(&self) -> i32 {
        self.root_pc
    }

    pub fn kind(&self) -> ScaleKind {
        self.kind
    }

    fn rebuild_mask(&mut self) {
        let root_pc = self.root_pc;
        let mut enabled_pc = [false; 12];
        match self.kind {
            ScaleKind::Chromatic => enabled_pc.fill(true),
            ScaleKind::Major => {
                for pc in [0, 2, 4, 5, 7, 9, 11] {
                    enabled_pc[((pc + root_pc).rem_euclid(12)) as usize] = true;
                }
            }
            ScaleKind::Minor => {
                for pc in [0, 2, 3, 5, 7, 8, 10] {
                    enabled_pc[((pc + root_pc).rem_euclid(12)) as usize] = true;
                }
            }
        }
        self.enabled_pc = enabled_pc;
    }

    pub fn map_hz_to_scale(&self, hz: f32) -> Option<f32> {
        if !hz.is_finite() || hz <= 0.0 {
            return None;
        }

        let midi = hz_to_midi(hz);
        let nearest = midi.round() as i32;

        for distance in 0..48 {
            let up = nearest + distance;
            if self.note_enabled(up) {
                return Some(midi_to_hz(up as f32));
            }
            if distance > 0 {
                let down = nearest - distance;
                if self.note_enabled(down) {
                    return Some(midi_to_hz(down as f32));
                }
            }
        }

        None
    }

    fn note_enabled(&self, midi_note: i32) -> bool {
        let pc = midi_note.rem_euclid(12) as usize;
        self.enabled_pc[pc]
    }
}

pub fn hz_to_midi(hz: f32) -> f32 {
    69.0 + 12.0 * (hz / 440.0).log2()
}

pub fn midi_to_hz(midi_note: f32) -> f32 {
    440.0 * 2.0_f32.powf((midi_note - 69.0) / 12.0)
}

pub fn parse_root_note(root: &str) -> Option<i32> {
    match root.trim().to_ascii_uppercase().as_str() {
        "C" => Some(0),
        "C#" | "DB" => Some(1),
        "D" => Some(2),
        "D#" | "EB" => Some(3),
        "E" => Some(4),
        "F" => Some(5),
        "F#" | "GB" => Some(6),
        "G" => Some(7),
        "G#" | "AB" => Some(8),
        "A" => Some(9),
        "A#" | "BB" => Some(10),
        "B" => Some(11),
        _ => None,
    }
}
