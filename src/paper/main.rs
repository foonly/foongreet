//! foonpaper: turns an image into a desktop wallpaper in the foongreet
//! style: low resolution, one phosphor color, scanlines and glow, but on a
//! flat screen rather than a curved tube.

use foongreet::{background, crt, crt::Crt, output::snapshot};
use std::{path::PathBuf, process::ExitCode};

const USAGE: &str = "\
usage: foonpaper [options] IN.png OUT.png

  --size WxH         output size in pixels (default 1920x1080); give
                     portrait screens as e.g. 1440x2560
  --phosphor COLOR   amber or green (default amber)
  --dim N            image brightness, 0.0 to 1.0 (default 0.5)
";

struct Args {
    input: PathBuf,
    out: PathBuf,
    size: (usize, usize),
    phosphor: crt::Phosphor,
    dim: f32,
}

fn parse_args() -> Result<Args, String> {
    let mut files = Vec::new();
    let mut size = (1920, 1080);
    let mut phosphor = crt::Phosphor::Amber;
    let mut dim = 0.5;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut val = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--size" => {
                let v = val()?;
                let (w, h) = v.split_once('x').ok_or("--size: expected WxH")?;
                size = (w.parse().map_err(|_| "--size: bad width")?, h.parse().map_err(|_| "--size: bad height")?);
                if size.0 == 0 || size.1 == 0 {
                    return Err("--size: must not be zero".into());
                }
            }
            "--phosphor" => {
                phosphor = match val()?.as_str() {
                    "amber" => crt::Phosphor::Amber,
                    "green" => crt::Phosphor::Green,
                    p => return Err(format!("--phosphor: unknown color {p}")),
                }
            }
            "--dim" => dim = val()?.parse::<f32>().map_err(|_| "--dim: expected a number")?.clamp(0.0, 1.0),
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            s if s.starts_with('-') => return Err(format!("unknown option {s}\n\n{USAGE}")),
            _ => files.push(PathBuf::from(arg)),
        }
    }
    let [input, out]: [PathBuf; 2] = files.try_into().map_err(|_| format!("expected IN.png and OUT.png\n\n{USAGE}"))?;
    Ok(Args { input, out, size, phosphor, dim })
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    crt::init_threads();
    let (w, h) = args.size;
    let mut crt = Crt::flat(w, h, args.phosphor);
    // A single frame: no flicker or rolling bar, and the noise stays put.
    crt.still = true;
    let lum = background::load(&args.input, crt.lw, crt.lh, args.dim)?;
    let mut px = vec![0u32; w * h];
    crt.render(&lum, 0.0, 0.0, &mut px, w);
    snapshot::save(&args.out, &px, w, h)
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("foonpaper: {e}");
            ExitCode::FAILURE
        }
    }
}
