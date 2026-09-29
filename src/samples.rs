use super::*;
use anyhow::{Context, bail};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct SampleSlice {
    data: Arc<Vec<f32>>,
    rate: u32,
    name: String,
    start: usize,
    end: usize,
}

impl SampleSlice {
    fn load(path: &str) -> Result<Self> {
        let mut reader = hound::WavReader::open(path).context("Cannot open WAV")?;
        let spec = reader.spec();
        if spec.channels == 0 || spec.sample_rate == 0 || !(1..=32).contains(&spec.bits_per_sample)
        {
            bail!("Unsupported WAV format");
        }
        // Bound decoded memory and keep accidental large imports manageable.
        if reader.len() > 20_000_000 {
            bail!("WAV too large (maximum 20 million channel samples)");
        }
        let raw: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Float => reader
                .samples::<f32>()
                .collect::<std::result::Result<_, _>>()?,
            hound::SampleFormat::Int => {
                let scale = 2_f32.powi(spec.bits_per_sample as i32 - 1);
                reader
                    .samples::<i32>()
                    .map(|v| v.map(|n| n as f32 / scale))
                    .collect::<std::result::Result<_, _>>()?
            }
        };
        if raw.is_empty()
            || raw.len() % spec.channels as usize != 0
            || raw.iter().any(|v| !v.is_finite())
        {
            bail!("WAV contains empty, incomplete, or invalid audio");
        }
        let data: Vec<f32> = raw
            .chunks_exact(spec.channels as usize)
            .map(|frame| {
                frame.iter().map(|v| v.clamp(-1.0, 1.0)).sum::<f32>() / spec.channels as f32
            })
            .collect();
        Ok(Self {
            end: data.len(),
            start: 0,
            data: Arc::new(data),
            rate: spec.sample_rate,
            name: Path::new(path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        })
    }

    fn split(&self) -> Option<[Self; LANES]> {
        let len = self.end - self.start;
        if len < LANES {
            return None;
        }
        Some(std::array::from_fn(|i| Self {
            start: self.start + len * i / LANES,
            end: self.start + len * (i + 1) / LANES,
            ..self.clone()
        }))
    }
}

#[derive(Default)]
pub struct SampleVoice {
    slice: Option<SampleSlice>,
    position: f64,
}

impl SampleVoice {
    pub fn new(slice: Option<SampleSlice>) -> Self {
        Self {
            slice,
            position: 0.0,
        }
    }

    pub fn next(&mut self, output_rate: f32) -> f32 {
        let Some(s) = &self.slice else {
            return 0.0;
        };
        let length = (s.end - s.start) as f64;
        if self.position >= length {
            return 0.0;
        }
        let index = s.start + self.position as usize;
        let fraction = self.position.fract() as f32;
        let value =
            s.data[index] * (1.0 - fraction) + s.data[(index + 1).min(s.end - 1)] * fraction;
        // Short fades prevent hard cut points from clicking.
        let fade = (s.rate as f64 * 0.002).min(length / 2.0).max(1.0);
        let gain = (self.position / fade)
            .min((length - self.position) / fade)
            .min(1.0) as f32;
        self.position += s.rate as f64 / output_rate as f64;
        value * gain
    }
}

pub struct SampleEditor {
    slice: Option<SampleSlice>,
    path: String,
    entering_path: bool,
    edit_end: bool,
    message: String,
}

impl SampleEditor {
    pub fn new(slice: Option<SampleSlice>) -> Self {
        Self {
            entering_path: slice.is_none(),
            slice,
            path: String::new(),
            edit_end: false,
            message: String::new(),
        }
    }
}

fn assign(app: &mut App, lane: usize, sample: Option<SampleSlice>) {
    app.samples[lane] = sample.clone();
    let _ = app.tx.send(EngineCmd::SetSample { lane, sample });
}

pub fn handle_editor_key(app: &mut App, key: KeyEvent) {
    let mut editor = app.sample_editor.take().unwrap();
    if key.code == KeyCode::Esc {
        let _ = app.tx.send(EngineCmd::Preview(None));
        return;
    }
    if editor.entering_path {
        match key.code {
            KeyCode::Char(c) => editor.path.push(c),
            KeyCode::Backspace => {
                editor.path.pop();
            }
            KeyCode::Enter => {
                let path = editor.path.trim().trim_matches('"').trim_matches('\'');
                let expanded = if let Some(rest) = path.strip_prefix("~/") {
                    std::env::var("HOME")
                        .map(|h| format!("{h}/{rest}"))
                        .unwrap_or_else(|_| path.into())
                } else {
                    path.into()
                };
                match SampleSlice::load(&expanded) {
                    Ok(slice) => {
                        editor.slice = Some(slice);
                        editor.entering_path = false;
                        editor.message.clear();
                    }
                    Err(e) => editor.message = format!("{e:#}"),
                }
            }
            _ => {}
        }
    } else {
        match key.code {
            KeyCode::Char('l') => {
                editor.entering_path = true;
                editor.path.clear();
            }
            KeyCode::Tab => editor.edit_end = !editor.edit_end,
            KeyCode::Left | KeyCode::Right => {
                if let Some(s) = &mut editor.slice {
                    let amount = if key.modifiers.contains(KeyModifiers::SHIFT) {
                        1
                    } else {
                        (s.rate as usize / 100).max(1)
                    };
                    let forward = key.code == KeyCode::Right;
                    let point = if editor.edit_end {
                        &mut s.end
                    } else {
                        &mut s.start
                    };
                    *point = if forward {
                        point.saturating_add(amount)
                    } else {
                        point.saturating_sub(amount)
                    };
                    if editor.edit_end {
                        s.end = s.end.clamp(s.start + 1, s.data.len());
                    } else {
                        s.start = s.start.min(s.end - 1);
                    }
                }
            }
            KeyCode::Char(' ') => {
                let _ = app.tx.send(EngineCmd::Preview(editor.slice.clone()));
            }
            KeyCode::Char('r') => {
                if let Some(s) = &mut editor.slice {
                    s.start = 0;
                    s.end = s.data.len();
                }
            }
            KeyCode::Char('d') => {
                assign(app, app.cursor_lane, None);
                app.status = "Restored synthesized drum on selected track".into();
                let _ = app.tx.send(EngineCmd::Preview(None));
                return;
            }
            KeyCode::Enter => {
                assign(app, app.cursor_lane, editor.slice.clone());
                app.status = format!("Chop assigned to track {}", app.cursor_lane + 1);
                let _ = app.tx.send(EngineCmd::Preview(None));
                return;
            }
            KeyCode::Char('a') => {
                if let Some(chops) = editor.slice.as_ref().and_then(SampleSlice::split) {
                    for (lane, chop) in chops.into_iter().enumerate() {
                        assign(app, lane, Some(chop));
                    }
                    app.status =
                        "Six equal chops assigned to all tracks; existing steps preserved".into();
                    let _ = app.tx.send(EngineCmd::Preview(None));
                    return;
                } else {
                    editor.message = "Select at least six sample frames to split".into();
                }
            }
            _ => {}
        }
    }
    app.sample_editor = Some(editor);
}

pub fn draw_editor(f: &mut ratatui::Frame, editor: &SampleEditor, lane: usize) {
    let area = centered_rect(88, 18, f.size());
    let mut lines = vec![Line::from(format!(
        "Track {} • WAV samples (stereo is mixed to mono)",
        lane + 1
    ))];
    if editor.entering_path {
        lines.push(Line::from(
            "Enter WAV path, then Enter to load. Esc cancels.",
        ));
        lines.push(Line::from(format!("> {}▏", editor.path)));
    } else if let Some(s) = &editor.slice {
        lines.push(Line::from(format!(
            "{} • {:.3}s • {} Hz",
            s.name,
            s.data.len() as f64 / s.rate as f64,
            s.rate
        )));
        let width = area.width.saturating_sub(4).max(1) as usize;
        let levels = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
        let waveform: Vec<Span> = (0..width)
            .map(|i| {
                let start = i * s.data.len() / width;
                let end = ((i + 1) * s.data.len() / width)
                    .max(start + 1)
                    .min(s.data.len());
                let peak = s.data[start..end].iter().fold(0_f32, |p, v| p.max(v.abs()));
                let selected = end > s.start && start < s.end;
                Span::styled(
                    levels[(peak * 8.0).round().min(8.0) as usize].to_string(),
                    Style::default().fg(if selected {
                        Color::Cyan
                    } else {
                        Color::DarkGray
                    }),
                )
            })
            .collect();
        lines.push(Line::from(""));
        lines.push(Line::from(waveform));
        lines.push(Line::from(format!(
            "{} Start {:.4}s    {} End {:.4}s    Length {:.4}s",
            if !editor.edit_end { ">" } else { " " },
            s.start as f64 / s.rate as f64,
            if editor.edit_end { ">" } else { " " },
            s.end as f64 / s.rate as f64,
            (s.end - s.start) as f64 / s.rate as f64
        )));
        lines.push(Line::from(
            "Tab: start/end   ←/→: 10ms   Shift+←/→: one frame",
        ));
        lines.push(Line::from(
            "Space: preview   r: reset range   l: load another WAV",
        ));
        lines.push(Line::from("Enter: assign selection to this track"));
        lines.push(Line::from(
            "a: split selection into 6 equal chops, replacing ALL track sounds",
        ));
        lines.push(Line::from(
            "d: restore this track's drum   Esc: cancel   Ctrl-C: quit",
        ));
    }
    lines.push(Line::styled(
        editor.message.clone(),
        Style::default().fg(Color::Yellow),
    ));
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(block().title(" Sample chopper ")),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SampleSlice {
        SampleSlice {
            data: Arc::new(vec![0.5; 103]),
            rate: 1000,
            name: "test.wav".into(),
            start: 7,
            end: 102,
        }
    }

    fn temp_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("tuityloops-{}-{label}.wav", std::process::id()))
    }

    #[test]
    fn chops_cover_selection_without_gaps_or_empty_slices() {
        let s = sample();
        let chops = s.split().unwrap();
        assert_eq!(chops[0].start, s.start);
        assert_eq!(chops[LANES - 1].end, s.end);
        for pair in chops.windows(2) {
            assert_eq!(pair[0].end, pair[1].start);
        }
        assert!(
            chops
                .iter()
                .all(|c| c.end > c.start && Arc::ptr_eq(&s.data, &c.data))
        );
        assert!(SampleSlice { end: 8, ..s }.split().is_none());
    }

    #[test]
    fn playback_resamples_and_never_reads_outside_chop() {
        let mut s = sample();
        let mut data = vec![1.0; 103];
        data[7..102].fill(0.5);
        s.data = Arc::new(data);
        let mut voice = SampleVoice::new(Some(s));
        let values: Vec<_> = (0..190).map(|_| voice.next(2000.0)).collect();
        assert!(values.iter().all(|v| *v >= 0.0 && *v <= 0.5));
        assert_eq!(values[10], 0.5);
        assert_eq!(voice.next(2000.0), 0.0);
    }

    #[test]
    fn loads_pcm_stereo_and_float_wav_and_rejects_empty() {
        for (label, format, bits) in [
            ("pcm", hound::SampleFormat::Int, 16),
            ("float", hound::SampleFormat::Float, 32),
        ] {
            let path = temp_path(label);
            let mut w = hound::WavWriter::create(
                &path,
                hound::WavSpec {
                    channels: 2,
                    sample_rate: 22050,
                    bits_per_sample: bits,
                    sample_format: format,
                },
            )
            .unwrap();
            if format == hound::SampleFormat::Int {
                w.write_sample(16384_i16).unwrap();
                w.write_sample(0_i16).unwrap();
            } else {
                w.write_sample(0.5_f32).unwrap();
                w.write_sample(0_f32).unwrap();
            }
            w.finalize().unwrap();
            let loaded = SampleSlice::load(path.to_str().unwrap()).unwrap();
            assert_eq!(loaded.rate, 22050);
            assert_eq!(loaded.data.as_slice(), &[0.25]);
            std::fs::remove_file(path).unwrap();
        }
        let path = temp_path("empty");
        hound::WavWriter::create(
            &path,
            hound::WavSpec {
                channels: 1,
                sample_rate: 44100,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap()
        .finalize()
        .unwrap();
        assert!(SampleSlice::load(path.to_str().unwrap()).is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn editor_bounds_cancel_and_path_input_are_isolated_from_sequencer() {
        let (mut app, _rx) = crate::tests::test_app();
        app.sample_editor = Some(SampleEditor::new(None));
        for c in "q p.wav".chars() {
            assert!(
                !handle_key(
                    &mut app,
                    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
                )
                .unwrap()
            );
        }
        assert_eq!(app.sample_editor.as_ref().unwrap().path, "q p.wav");
        assert!(!app.playing);
        app.sample_editor = Some(SampleEditor::new(Some(sample())));
        for _ in 0..200 {
            handle_editor_key(&mut app, KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        }
        let s = app.sample_editor.as_ref().unwrap().slice.as_ref().unwrap();
        assert_eq!(s.start, s.end - 1);
        handle_editor_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.samples.iter().all(Option::is_none));
    }

    #[test]
    fn auto_chops_sync_to_engine_and_export() {
        let (mut app, rx) = crate::tests::test_app();
        app.sample_editor = Some(SampleEditor::new(Some(sample())));
        handle_editor_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        assert!(app.sample_editor.is_none());
        assert!(app.samples.iter().all(Option::is_some));
        let mut engine = EngineState::new(44100.0, Arc::new(AtomicUsize::new(0)));
        engine.drain(&rx);
        assert!(engine.samples.iter().all(Option::is_some));
        app.pat.grid[0][0] = true;
        let path = temp_path("export");
        render_wav(
            &app.pat,
            &app.samples,
            app.master_gain,
            path.to_str().unwrap(),
        )
        .unwrap();
        let mut reader = hound::WavReader::open(&path).unwrap();
        let audio: Vec<i16> = reader.samples().map(Result::unwrap).collect();
        assert_eq!(audio.len(), 44100 * 8); // two bars at 120 BPM, stereo
        assert!(audio.iter().any(|v| *v > 0));
        assert!(audio[44100..].iter().all(|v| *v == 0));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn editor_renders_on_small_and_normal_terminals() {
        for (width, height) in [(20, 8), (100, 30)] {
            let backend = ratatui::backend::TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            let editor = SampleEditor::new(Some(sample()));
            terminal.draw(|f| draw_editor(f, &editor, 0)).unwrap();
        }
    }
}
