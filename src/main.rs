use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Terminal,
};
use std::{
    io,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

const STEPS: usize = 16;
const LANES: usize = 4;

#[derive(Clone, Copy)]
struct Lane {
    name: &'static str,
}

#[derive(Clone)]
struct Pattern {
    bpm: f32,
    grid: [[bool; STEPS]; LANES],
}

#[derive(Debug, Clone)]
enum EngineCmd {
    SetPlaying(bool),
    SetBpm(f32),
    ToggleStep { lane: usize, step: usize, on: bool },
    SetMasterGain(f32),
    // later: lane gain/pan, sample load, etc.
}

struct App {
    lanes: [Lane; LANES],
    pat: Pattern,

    playing: bool,
    master_gain: f32,

    cursor_lane: usize,
    cursor_step: usize,

    // playhead from audio thread
    playhead_step: Arc<AtomicUsize>,

    // ui -> audio
    tx: crossbeam_channel::Sender<EngineCmd>,
}

fn main() -> Result<()> {
    // ---- UI + terminal init ----
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // ---- Engine init ----
    let (tx, rx) = crossbeam_channel::unbounded::<EngineCmd>();
    let playhead_step = Arc::new(AtomicUsize::new(0));
    let _audio = AudioEngine::start(rx, playhead_step.clone())?;

    // ---- App init ----
    let lanes = [
        Lane { name: "Kick" },
        Lane { name: "Snare" },
        Lane { name: "Hat" },
        Lane { name: "Clap" },
    ];

    let mut pat = Pattern {
        bpm: 120.0,
        grid: [[false; STEPS]; LANES],
    };

    // default beat
    for &s in &[0, 4, 8, 12] {
        pat.grid[0][s] = true;
    } // kick
    for &s in &[4, 12] {
        pat.grid[1][s] = true;
    } // snare
    for s in 0..STEPS {
        pat.grid[2][s] = true;
    } // hats

    let mut app = App {
        lanes,
        pat,
        playing: false,
        master_gain: 0.8,
        cursor_lane: 0,
        cursor_step: 0,
        playhead_step,
        tx,
    };

    // make sure audio engine matches initial UI state
    let _ = app.tx.send(EngineCmd::SetBpm(app.pat.bpm));
    let _ = app.tx.send(EngineCmd::SetMasterGain(app.master_gain));
    // Sync initial pattern to audio engine
    for lane in 0..LANES {
        for step in 0..STEPS {
            if app.pat.grid[lane][step] {
                let _ = app.tx.send(EngineCmd::ToggleStep {
                    lane,
                    step,
                    on: true,
                });
            }
        }
    }
    // ---- Main loop ----
    let res = run_app(&mut terminal, &mut app);

    // ---- Cleanup ----
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    res
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|f| draw_ui(f, app))?;

        if event::poll(Duration::from_millis(16))? {
            if let Event::Key(k) = event::read()? {
                if handle_key(app, k)? {
                    break;
                }
            }
        }
    }
    Ok(())
}

fn handle_key(app: &mut App, k: KeyEvent) -> Result<bool> {
    match (k.code, k.modifiers) {
        (KeyCode::Char('q'), _) => return Ok(true),
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Ok(true),

        (KeyCode::Char('p'), _) => {
            app.playing = !app.playing;
            let _ = app.tx.send(EngineCmd::SetPlaying(app.playing));
        }

        (KeyCode::Up, _) => app.cursor_lane = app.cursor_lane.saturating_sub(1),
        (KeyCode::Down, _) => app.cursor_lane = (app.cursor_lane + 1).min(LANES - 1),
        (KeyCode::Left, _) => app.cursor_step = app.cursor_step.saturating_sub(1),
        (KeyCode::Right, _) => app.cursor_step = (app.cursor_step + 1).min(STEPS - 1),

        (KeyCode::Char(' '), _) => {
            let lane = app.cursor_lane;
            let step = app.cursor_step;
            let on = !app.pat.grid[lane][step];
            app.pat.grid[lane][step] = on;
            let _ = app.tx.send(EngineCmd::ToggleStep { lane, step, on });
        }

        (KeyCode::Char('+') | KeyCode::Char('='), _) => {
            app.pat.bpm = (app.pat.bpm + 1.0).min(300.0);
            let _ = app.tx.send(EngineCmd::SetBpm(app.pat.bpm));
        }
        (KeyCode::Char('-'), _) => {
            app.pat.bpm = (app.pat.bpm - 1.0).max(30.0);
            let _ = app.tx.send(EngineCmd::SetBpm(app.pat.bpm));
        }

        (KeyCode::Char(']'), _) => {
            app.master_gain = (app.master_gain + 0.05).min(1.0);
            let _ = app.tx.send(EngineCmd::SetMasterGain(app.master_gain));
        }
        (KeyCode::Char('['), _) => {
            app.master_gain = (app.master_gain - 0.05).max(0.0);
            let _ = app.tx.send(EngineCmd::SetMasterGain(app.master_gain));
        }

        (KeyCode::Char('r'), _) => {
            // Optional: offline render still useful
            render_wav(&app.pat, "out.wav")?;
        }

        _ => {}
    }
    Ok(false)
}

fn draw_ui(f: &mut ratatui::Frame, app: &App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(10), Constraint::Length(2)])
        .split(f.size());

    let step = app.playhead_step.load(Ordering::Relaxed) % STEPS;

    // Header
    let header = Paragraph::new(Line::from(vec![
        Span::raw("BPM: "),
        Span::styled(format!("{:.0}", app.pat.bpm), Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("   Playing: "),
        Span::styled(
            if app.playing { "Yes" } else { "No" },
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("   Step: {:02X}", step)),
        Span::raw("   Master: "),
        Span::styled(
            format!("{:.2}", app.master_gain),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ]))
    .block(Block::default().borders(Borders::ALL).title("Beat TUI (Realtime)"));
    f.render_widget(header, root[0]);

    // Main grid
    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(14), Constraint::Min(10)])
        .split(root[1]);

    let lane_lines = app
        .lanes
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let sel = if i == app.cursor_lane { ">" } else { " " };
            Line::from(format!("{sel} {}", l.name))
        })
        .collect::<Vec<_>>();

    let lane_panel = Paragraph::new(lane_lines)
        .block(Block::default().borders(Borders::ALL).title("Lanes"));
    f.render_widget(lane_panel, main[0]);

    let mut lines: Vec<Line> = Vec::new();

    let header_steps = (0..STEPS)
        .map(|s| Span::styled(format!(" {s:X}"), Style::default().add_modifier(Modifier::DIM)))
        .collect::<Vec<_>>();
    lines.push(Line::from(header_steps));

    for lane in 0..LANES {
        let mut spans: Vec<Span> = Vec::new();
        for st in 0..STEPS {
            let on = app.pat.grid[lane][st];
            let is_cursor = lane == app.cursor_lane && st == app.cursor_step;
            let is_playhead = app.playing && st == step;

            let ch = if on { "■" } else { "·" };
            let mut style = Style::default();
            if is_playhead {
                style = style.add_modifier(Modifier::BOLD);
            }
            if is_cursor {
                style = style.add_modifier(Modifier::REVERSED);
            }
            spans.push(Span::styled(format!(" {ch}"), style));
        }
        lines.push(Line::from(spans));
    }

    let grid = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("Steps"));
    f.render_widget(grid, main[1]);

    let footer = Paragraph::new(
        "Controls: arrows=move  space=toggle  p=play  +/- BPM  [ ] master  r=render out.wav  q=quit",
    )
    .block(Block::default().borders(Borders::ALL));
    f.render_widget(footer, root[2]);
}


struct AudioEngine {
    _stream: cpal::Stream,
}

impl AudioEngine {
    fn start(
        rx: crossbeam_channel::Receiver<EngineCmd>,
        playhead_step: Arc<AtomicUsize>,
    ) -> Result<Self> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow::anyhow!("no output device"))?;

        let config = device.default_output_config()?;
        let sample_rate = config.sample_rate().0 as f32;
        let channels = config.channels() as usize;

        let mut state = EngineState::new(sample_rate, playhead_step);

        let err_fn = |e| eprintln!("audio error: {e}");

        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => device.build_output_stream(
                &config.into(),
                move |out: &mut [f32], _| {
                    state.drain(&rx);
                    state.render_f32(out, channels);
                },
                err_fn,
                None,
            )?,
            cpal::SampleFormat::I16 => device.build_output_stream(
                &config.into(),
                move |out: &mut [i16], _| {
                    state.drain(&rx);
                    state.render_i16(out, channels);
                },
                err_fn,
                None,
            )?,
            cpal::SampleFormat::U16 => device.build_output_stream(
                &config.into(),
                move |out: &mut [u16], _| {
                    state.drain(&rx);
                    state.render_u16(out, channels);
                },
                err_fn,
                None,
            )?,
            _ => return Err(anyhow::anyhow!("unsupported sample format")),
        };

        stream.play()?;
        Ok(Self { _stream: stream })
    }
}

struct EngineState {
    sr: f32,
    playing: bool,
    bpm: f32,
    master_gain: f32,

    grid: [[bool; STEPS]; LANES],

    // timing
    samples_per_step: f32,
    step_phase: f32,
    step_index: usize,

    // drum voices
    kick: DrumKick,
    snare: DrumSnare,
    hat: DrumHat,
    clap: DrumClap,

    playhead_step: Arc<AtomicUsize>,
}

impl EngineState {
    fn new(sr: f32, playhead_step: Arc<AtomicUsize>) -> Self {
        let bpm = 120.0;
        let samples_per_step = bpm_to_samples_per_step(sr, bpm);

        Self {
            sr,
            playing: false,
            bpm,
            master_gain: 0.8,
            grid: [[false; STEPS]; LANES],

            samples_per_step,
            step_phase: 0.0,
            step_index: 0,

            kick: DrumKick::new(sr),
            snare: DrumSnare::new(sr),
            hat: DrumHat::new(sr),
            clap: DrumClap::new(sr),

            playhead_step,
        }
    }

    fn drain(&mut self, rx: &crossbeam_channel::Receiver<EngineCmd>) {
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                EngineCmd::SetPlaying(p) => {
                    if p && !self.playing {
                        // starting playback, reset timing to stay in sync with UI
                        self.step_phase = self.samples_per_step;
                    }
                    self.playing = p;
                    if !p {
                        self.step_phase = 0.0;
                        self.step_index = 0;
                        self.playhead_step.store(0, Ordering::Relaxed);
                        self.kick.reset();
                        self.snare.reset();
                        self.hat.reset();
                        self.clap.reset();
                    }
                }
                EngineCmd::SetBpm(b) => {
                    self.bpm = b.clamp(30.0, 300.0);
                    self.samples_per_step = bpm_to_samples_per_step(self.sr, self.bpm);
                }
                EngineCmd::ToggleStep { lane, step, on } => {
                    if lane < LANES && step < STEPS {
                        self.grid[lane][step] = on;
                    }
                }
                EngineCmd::SetMasterGain(g) => self.master_gain = g.clamp(0.0, 1.0),
            }
        }
    }

    fn advance_steps(&mut self, frames: usize) {
        if !self.playing {
            return;
        }
        self.step_phase += frames as f32;

        while self.step_phase >= self.samples_per_step {
            self.step_phase -= self.samples_per_step;
            self.step_index = (self.step_index + 1) % STEPS;
            self.playhead_step.store(self.step_index, Ordering::Relaxed);

            // trigger on step boundary
            if self.grid[0][self.step_index] {
                self.kick.trigger();
            }
            if self.grid[1][self.step_index] {
                self.snare.trigger();
            }
            if self.grid[2][self.step_index] {
                self.hat.trigger();
            }
            if self.grid[3][self.step_index] {
                self.clap.trigger();
            }
        }
    }

    fn next_sample(&mut self) -> f32 {
        let s = self.kick.next()
            + self.snare.next()
            + self.hat.next()
            + self.clap.next();
        (s.tanh()) * self.master_gain
    }

    fn render_f32(&mut self, out: &mut [f32], ch: usize) {
        let frames = out.len() / ch;
        self.advance_steps(frames);
        for i in 0..frames {
            let s = self.next_sample();
            for c in 0..ch {
                out[i * ch + c] = s;
            }
        }
    }

    fn render_i16(&mut self, out: &mut [i16], ch: usize) {
        let frames = out.len() / ch;
        self.advance_steps(frames);
        for i in 0..frames {
            let s = self.next_sample().clamp(-1.0, 1.0);
            let v = (s * i16::MAX as f32) as i16;
            for c in 0..ch {
                out[i * ch + c] = v;
            }
        }
    }

    fn render_u16(&mut self, out: &mut [u16], ch: usize) {
        let frames = out.len() / ch;
        self.advance_steps(frames);
        for i in 0..frames {
            let s = self.next_sample().clamp(-1.0, 1.0);
            let u = ((s * 0.5 + 0.5) * u16::MAX as f32) as u16;
            for c in 0..ch {
                out[i * ch + c] = u;
            }
        }
    }
}

fn bpm_to_samples_per_step(sr: f32, bpm: f32) -> f32 {
    // 4/4, 16 steps per bar => step = 1/4 beat
    let sec_per_beat = 60.0 / bpm;
    let sec_per_step = sec_per_beat / 4.0;
    sec_per_step * sr
}


struct DrumKick {
    sr: f32,
    t: f32,
    env: f32,
    phase: f32,
}
impl DrumKick {
    fn new(sr: f32) -> Self {
        Self { sr, t: 0.0, env: 0.0, phase: 0.0 }
    }
    fn trigger(&mut self) {
        self.t = 0.0;
        self.env = 1.0;
    }
    fn reset(&mut self) { self.env = 0.0; self.t = 0.0; }
    fn next(&mut self) -> f32 {
        if self.env <= 0.0001 { return 0.0; }
        let freq = 90.0 + 140.0 * (-self.t * 18.0).exp();
        self.phase += 2.0 * std::f32::consts::PI * freq / self.sr;
        if self.phase > 2.0 * std::f32::consts::PI { self.phase -= 2.0 * std::f32::consts::PI; }
        let s = self.phase.sin();
        self.env *= 0.9975;
        self.t += 1.0 / self.sr;
        s * self.env * 1.2
    }
}

struct DrumSnare {
    sr: f32,
    env: f32,
    noise: u32,
    tone_phase: f32,
}
impl DrumSnare {
    fn new(sr: f32) -> Self {
        Self { sr, env: 0.0, noise: 0x12345678, tone_phase: 0.0 }
    }
    fn trigger(&mut self) { self.env = 1.0; }
    fn reset(&mut self) { self.env = 0.0; }
    fn next(&mut self) -> f32 {
        if self.env <= 0.0001 { return 0.0; }
        self.noise = self.noise.wrapping_mul(1664525).wrapping_add(1013904223);
        let n = ((self.noise >> 9) as f32 / (u32::MAX >> 9) as f32) * 2.0 - 1.0;

        let tone_f = 190.0;
        self.tone_phase += 2.0 * std::f32::consts::PI * tone_f / self.sr;
        if self.tone_phase > 2.0 * std::f32::consts::PI { self.tone_phase -= 2.0 * std::f32::consts::PI; }
        let tone = self.tone_phase.sin() * 0.25;

        self.env *= 0.994;
        (n * 0.8 + tone) * self.env * 0.8
    }
}

struct DrumHat {
    sr: f32,
    env: f32,
    noise: u32,
}
impl DrumHat {
    fn new(sr: f32) -> Self { Self { sr, env: 0.0, noise: 0xdeadbeef } }
    fn trigger(&mut self) { self.env = 1.0; }
    fn reset(&mut self) { self.env = 0.0; }
    fn next(&mut self) -> f32 {
        if self.env <= 0.0001 { return 0.0; }
        self.noise = self.noise.wrapping_mul(1103515245).wrapping_add(12345);
        let n = ((self.noise >> 10) as f32 / (u32::MAX >> 10) as f32) * 2.0 - 1.0;
        self.env *= 0.985;
        n * self.env * 0.25
    }
}

struct DrumClap {
    sr: f32,
    env: f32,
    noise: u32,
    burst_phase: usize,
}
impl DrumClap {
    fn new(sr: f32) -> Self { Self { sr, env: 0.0, noise: 0xa5a5a5a5, burst_phase: 0 } }
    fn trigger(&mut self) { self.env = 1.0; self.burst_phase = 0; }
    fn reset(&mut self) { self.env = 0.0; self.burst_phase = 0; }
    fn next(&mut self) -> f32 {
        if self.env <= 0.0001 { return 0.0; }
        self.noise = self.noise.wrapping_mul(22695477).wrapping_add(1);
        let n = ((self.noise >> 11) as f32 / (u32::MAX >> 11) as f32) * 2.0 - 1.0;

        let gate = match self.burst_phase {
            0..=250 => 1.0,
            251..=380 => 0.0,
            381..=620 => 1.0,
            621..=760 => 0.0,
            761..=1100 => 1.0,
            _ => 0.0,
        };
        self.burst_phase += 1;

        self.env *= 0.992;
        n * self.env * gate * 0.5
    }
}


fn render_wav(pat: &Pattern, path: &str) -> Result<()> {
    let sample_rate = 44_100u32;
    let seconds = 4.0;
    let frames = (seconds * sample_rate as f32) as usize;

    let spec = hound::WavSpec {
        channels: 2,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec)?;

    let bpm = pat.bpm;
    let sec_per_beat = 60.0 / bpm;
    let sec_per_step = sec_per_beat / 4.0;
    let samples_per_step = (sec_per_step * sample_rate as f32) as usize;

    let mut kick = DrumKick::new(sample_rate as f32);
    let mut snare = DrumSnare::new(sample_rate as f32);
    let mut hat = DrumHat::new(sample_rate as f32);
    let mut clap = DrumClap::new(sample_rate as f32);

    for i in 0..frames {
        let step = (i / samples_per_step) % STEPS;
        let step_start = (i % samples_per_step) == 0;

        if step_start {
            if pat.grid[0][step] { kick.trigger(); }
            if pat.grid[1][step] { snare.trigger(); }
            if pat.grid[2][step] { hat.trigger(); }
            if pat.grid[3][step] { clap.trigger(); }
        }

        let s = (kick.next() + snare.next() + hat.next() + clap.next()).tanh() * 0.7;
        let v = (s * i16::MAX as f32) as i16;
        w.write_sample(v)?;
        w.write_sample(v)?;
    }

    w.finalize()?;
    Ok(())
}