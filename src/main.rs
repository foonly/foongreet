mod auth;
mod background;
mod config;
mod crt;
mod font;
mod output;
mod power;
mod sessions;
mod state;
mod tty;
mod ui;

use output::Output;
use std::{
    path::PathBuf,
    process::ExitCode,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

const FRAME: Duration = Duration::from_millis(33);
const POWER_OFF: f32 = 0.45;

const USAGE: &str = "\
usage: foongreet [options]

Settings live in the config file; see config.example.toml.

  --config FILE    config file (default /etc/foongreet/config.toml)
  --list-outputs   show connected screens and which one gets the login
  --mock           fake greetd (password: \"password\"); implies --dev
  --dev            Ctrl+C quits (raw mode otherwise swallows it); power
                   keys only show their command (also with --mock/--preview)
  --preview WxH    render in a desktop window (implies --mock unless
                   GREETD_SOCK is set; needs the `preview` feature)
  --snapshot FILE  render offscreen at the --preview size (default
                   1920x1080) for ~3s with --mock, then save a PNG
";

struct Args {
    config: PathBuf,
    list_outputs: bool,
    mock: bool,
    dev: bool,
    preview: Option<(usize, usize)>,
    snapshot: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        config: config::DEFAULT_PATH.into(),
        list_outputs: false,
        mock: false,
        dev: false,
        preview: None,
        snapshot: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut val = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--config" => a.config = val()?.into(),
            "--list-outputs" => a.list_outputs = true,
            "--mock" => a.mock = true,
            "--dev" => a.dev = true,
            "--preview" => {
                let v = val()?;
                let (w, h) = v.split_once('x').ok_or("--preview: expected WxH")?;
                a.preview = Some((
                    w.parse().map_err(|_| "--preview: bad width")?,
                    h.parse().map_err(|_| "--preview: bad height")?,
                ));
                a.mock |= std::env::var_os("GREETD_SOCK").is_none();
            }
            "--snapshot" => {
                a.snapshot = Some(val()?.into());
                a.mock = true;
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            _ => return Err(format!("unknown option {arg}\n\n{USAGE}")),
        }
    }
    Ok(a)
}

fn open_output(args: &Args, display: &config::Display) -> Result<Box<dyn Output>, String> {
    if args.snapshot.is_some() {
        let (w, h) = args.preview.unwrap_or((1920, 1080));
        return Ok(Box::new(output::snapshot::Snapshot::new(w, h)));
    }
    if let Some((w, h)) = args.preview {
        #[cfg(feature = "preview")]
        return output::preview::Preview::open(w, h)
            .map(|p| Box::new(p) as Box<dyn Output>)
            .map_err(|e| e.to_string());
        #[cfg(not(feature = "preview"))]
        {
            let _ = (w, h);
            return Err("built without the `preview` feature".into());
        }
    }
    if !display.fbdev {
        // Other screens: an empty tube, just the glass glow and vignette.
        let mut blank = |w, h, buf: &mut [u32], stride| {
            let mut crt = crt::Crt::new(w, h, display.phosphor);
            let lum = vec![0; crt.lw * crt.lh];
            crt.render(&lum, 0.0, 0.0, buf, stride);
        };
        match output::kms::Kms::open(display.output.as_deref(), &mut blank) {
            Ok(k) => return Ok(Box::new(k)),
            Err(e) => eprintln!("foongreet: KMS unavailable ({e}), falling back to fbdev"),
        }
    }
    output::fbdev::Fbdev::open().map(|f| Box::new(f) as Box<dyn Output>).map_err(|e| format!("fbdev: {e}"))
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    tty::install_signal_handlers();

    // Problems that should not stop the greeter: logged, and the first one
    // is shown on screen.
    let mut warnings: Vec<(String, String)> = Vec::new();
    let (config, warning) = config::load(&args.config);
    warnings.extend(warning.map(|w| (w, "CONFIG ERROR, USING DEFAULTS".into())));

    if args.list_outputs {
        if let Some((detail, _)) = warnings.first() {
            eprintln!("foongreet: {detail}");
        }
        for line in output::kms::Kms::list(config.display.output.as_deref()).map_err(|e| e.to_string())? {
            println!("{line}");
        }
        return Ok(());
    }

    let auth = if args.mock { auth::Auth::mock() } else { auth::Auth::connect()? };
    let font = match font::Font::load(&config.display.font) {
        Ok(font) => font,
        Err(e) => {
            warnings.push((e, "FONT NOT LOADED, USING DEFAULT".into()));
            font::Font::load(config::DEFAULT_FONT.as_ref())?
        }
    };
    let sessions = sessions::discover(&config.sessions);
    let state = state::State::load(config.login.cache_dir.clone());
    let power = power::Power::new(
        sessions::split_words(&config.power.poweroff),
        sessions::split_words(&config.power.reboot),
        args.dev || args.mock || args.preview.is_some(),
    );

    // Declared before `out` so the display is released before the VT goes
    // back to text mode.
    let mut tty = match (&args.preview, &args.snapshot) {
        (None, None) => Some(tty::Tty::open(true).map_err(|e| format!("tty: {e}"))?),
        _ => None,
    };
    let mut out = open_output(&args, &config.display)?;
    warnings.extend(out.warning().map(|w| (w.clone(), w)));

    let (w, h) = out.size();
    let mut crt = crt::Crt::new(w, h, config.display.phosphor);
    let bg = config.display.background.as_ref().and_then(|path| {
        background::load(path, crt.lw, crt.lh, config.display.background_dim)
            .map_err(|e| warnings.push((e, "BACKGROUND NOT LOADED".into())))
            .ok()
    });
    let mut ui = ui::Ui::new(font, bg, crt.lw, crt.lh, sessions, config.login.default_user.clone(), state, power);
    for (detail, _) in &warnings {
        eprintln!("foongreet: {detail}");
    }
    if let Some((_, short)) = warnings.first() {
        ui.warn(short);
    }
    let mut lum = vec![0u8; crt.lw * crt.lh];

    let mut next = Instant::now();
    let mut off_at: Option<f32> = None;
    loop {
        let keys = match &mut tty {
            Some(t) => t.keys(next.saturating_duration_since(Instant::now())),
            None => out.keys(next.saturating_duration_since(Instant::now())),
        };
        if off_at.is_none() {
            for k in keys {
                ui.key(k, &auth);
            }
            ui.tick();
            while let Ok(resp) = auth.rx.try_recv() {
                ui.response(resp, &auth);
            }
        }

        if let Some(path) = &args.snapshot {
            if ui.time() > 3.0 {
                let (w, h) = out.size();
                let mut px = vec![0u32; w * h];
                crt.render(&lum, ui.time(), 0.0, &mut px, w);
                return output::snapshot::save(path, &px, w, h);
            }
        }

        let quit = tty::QUIT.load(Ordering::SeqCst) || !out.is_open() || (ui.interrupted && (args.dev || args.mock));
        if quit && off_at.is_none() && !ui.done {
            break;
        }
        if ui.done && off_at.is_none() {
            off_at = Some(ui.time());
        }

        if Instant::now() >= next {
            let t = ui.time();
            let collapse = off_at.map_or(0.0, |o| ((t - o) / POWER_OFF).min(1.0));
            if collapse >= 1.0 {
                break;
            }
            ui.draw(&mut lum);
            out.present(&mut |buf, stride| crt.render(&lum, t, collapse, buf, stride)).map_err(|e| e.to_string())?;
            next += FRAME;
            if next < Instant::now() {
                next = Instant::now() + FRAME;
            }
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("foongreet: {e}");
            ExitCode::FAILURE
        }
    }
}
