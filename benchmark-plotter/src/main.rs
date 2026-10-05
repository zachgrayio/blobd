use ahash::HashMap;
use ahash::HashMapExt;
use benchmark_types::*;
use maud::html;
use maud::Markup;
use maud::PreEscaped;
use once_cell::sync::Lazy;
use plotters::prelude::*;
use regex::Regex;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

static FILENAME_RE: Lazy<Regex> =
  Lazy::new(|| Regex::new(r"^results\.(\d+)o\.(\d+)b\.json$").unwrap());

#[derive(Serialize)]
struct Dataset {
  object_size: u64,
  results: Vec<BenchmarkResults>,
}

#[derive(Deserialize)]
struct SuiteSample {
  profile: String,
  mode: String,
  result: BenchmarkResults,
}

fn collect_metrics(path: &str, value: &Value, metrics: &mut BTreeMap<String, f64>) {
  match value {
    Value::Number(number) => {
      if let Some(value) = number.as_f64() {
        metrics.insert(path.to_owned(), value);
      }
    }
    Value::Object(fields) => {
      for (name, value) in fields {
        collect_metrics(&format!("{path}.{name}"), value, metrics);
      }
    }
    _ => {}
  }
}

fn sample_metrics(result: &BenchmarkResults) -> BTreeMap<String, f64> {
  let value = serde_json::to_value(result).unwrap();
  let mut metrics = BTreeMap::new();
  for section in ["op", "store_metrics", "system_metrics", "cache_pressure"] {
    collect_metrics(section, &value[section], &mut metrics);
  }
  collect_metrics(
    "wait_for_end_secs",
    &value["wait_for_end_secs"],
    &mut metrics,
  );
  for operation in [
    "create",
    "write",
    "commit",
    "inspect",
    "random_read",
    "read",
    "delete",
  ] {
    if let Some(seconds) = value["op"][operation]["exec_secs"].as_f64() {
      if seconds > 0.0 {
        metrics.insert(
          format!("op.{operation}.ops_per_sec"),
          result.objects as f64 / seconds,
        );
      }
    }
  }
  metrics
}

fn render_metric(
  samples: &[(SuiteSample, BTreeMap<String, f64>)],
  metric: &str,
  path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
  let mut groups: BTreeMap<(&str, &str), Vec<f64>> = BTreeMap::new();
  for (sample, values) in samples {
    if let Some(&value) = values.get(metric) {
      groups
        .entry((&sample.profile, &sample.mode))
        .or_default()
        .push(value);
    }
  }
  let height = 120 + groups.len() as u32 * 42;
  let root = SVGBackend::new(path, (1400, height)).into_drawing_area();
  root.fill(&WHITE)?;
  root.draw(&Text::new(
    metric.to_string(),
    (32, 44),
    ("sans-serif", 26).into_font(),
  ))?;
  root.draw(&Text::new(
    "Median bar; line shows min to max across runs",
    (32, 72),
    ("sans-serif", 15).into_font(),
  ))?;
  let maximum = groups
    .values()
    .flat_map(|values| values.iter().copied())
    .fold(0.0f64, f64::max);
  let scale = if maximum > 0.0 { maximum } else { 1.0 };
  for (index, ((profile, mode), mut values)) in groups.into_iter().enumerate() {
    values.sort_by(f64::total_cmp);
    let median = values[values.len() / 2];
    let y = 102 + index as i32 * 42;
    let position = |value: f64| 330 + (value / scale * 850.0) as i32;
    let color = match mode {
      "baseline" => RGBColor(65, 91, 115),
      "clock" => RGBColor(24, 129, 103),
      _ => RGBColor(190, 113, 45),
    };
    root.draw(&Text::new(
      format!("{profile} / {mode}"),
      (32, y + 5),
      ("sans-serif", 16).into_font(),
    ))?;
    root.draw(&Rectangle::new(
      [(330, y - 9), (position(median), y + 9)],
      color.filled(),
    ))?;
    root.draw(&PathElement::new(
      vec![
        (position(values[0]), y),
        (position(*values.last().unwrap()), y),
      ],
      BLACK.stroke_width(2),
    ))?;
    root.draw(&Text::new(
      format!("{median:.3}"),
      (position(median).max(330) + 12, y + 5),
      ("sans-serif", 15).into_font(),
    ))?;
  }
  root.present()?;
  Ok(())
}

fn render_suite(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
  let samples: Vec<_> = fs::read_to_string(path)?
    .lines()
    .map(|line| -> Result<_, serde_json::Error> {
      let sample: SuiteSample = serde_json::from_str(line)?;
      let metrics = sample_metrics(&sample.result);
      Ok((sample, metrics))
    })
    .collect::<Result<_, _>>()?;
  if samples.is_empty() {
    return Err("suite has no samples".into());
  }
  let output = path.parent().unwrap().join("plots");
  fs::create_dir_all(&output)?;
  let names: BTreeSet<_> = samples
    .iter()
    .flat_map(|(_, metrics)| metrics.keys().cloned())
    .collect();
  let mut sections: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
  for name in names {
    let filename = format!("{}.svg", name.replace('.', "--"));
    render_metric(&samples, &name, &output.join(&filename))?;
    sections
      .entry(name.split('.').next().unwrap().to_owned())
      .or_default()
      .push((name, filename));
  }
  let page = html! {
    (maud::DOCTYPE)
    html lang="en" {
      head {
        meta charset="UTF-8" {}
        meta name="viewport" content="width=device-width, initial-scale=1.0" {}
        title { "CLOCK benchmark suite" }
        style { (PreEscaped(include_str!("index.css"))) }
      }
      body {
        main.container {
          h1 { "CLOCK benchmark suite" }
          p { (samples.len()) " recorded runs; bars show medians, lines show the range." }
          @for (section, charts) in sections {
            section.section {
              h2.section-title { (section) }
              @for (name, filename) in charts {
                figure.chart-container {
                  h3 { (name) }
                  img src=(filename) alt=(name) style="width:100%;height:auto" {}
                }
              }
            }
          }
        }
      }
    }
  };
  fs::write(output.join("index.html"), page.into_string())?;
  println!("Generated {}", output.join("index.html").display());
  Ok(())
}

fn parse_filename(filename: &str) -> Option<(u64, u64)> {
  let caps = FILENAME_RE.captures(filename)?;
  let objects = caps.get(1)?.as_str().parse().ok()?;
  let object_size = caps.get(2)?.as_str().parse().ok()?;
  Some((objects, object_size))
}

fn load_results() -> Vec<Dataset> {
  let cfg_dir = PathBuf::from("../benchmark-runner/cfg");
  let mut results_by_size: HashMap<u64, Vec<BenchmarkResults>> = HashMap::new();

  for entry in fs::read_dir(&cfg_dir).unwrap() {
    let entry = entry.unwrap();
    if !entry.file_type().unwrap().is_dir() {
      continue;
    }

    let benchmark_dir = entry.path();
    for file_entry in fs::read_dir(&benchmark_dir).unwrap() {
      let file_entry = file_entry.unwrap();
      let filename = file_entry.file_name().to_string_lossy().to_string();

      let Some((_, object_size)) = parse_filename(&filename) else {
        continue;
      };

      let content = fs::read_to_string(file_entry.path()).unwrap();
      let result: BenchmarkResults = serde_json::from_str(&content).unwrap();
      results_by_size
        .entry(object_size)
        .or_insert_with(Vec::new)
        .push(result);
    }
  }

  results_by_size
    .into_iter()
    .map(|(object_size, results)| Dataset {
      object_size,
      results,
    })
    .collect()
}

fn generate_html(datasets: Vec<Dataset>) -> Markup {
  let json_data = serde_json::to_string(&datasets).unwrap();
  // Escape for JavaScript string literal: escape backslashes and quotes
  let json_escaped = json_data
    .replace('\\', r#"\\"#)
    .replace('"', r#"\""#)
    .replace('\n', r#"\n"#)
    .replace('\r', r#"\r"#)
    .replace('\t', r#"\t"#);

  html! {
    (maud::DOCTYPE)
    html lang="en" {
      head {
        meta charset="UTF-8" {}
        meta name="viewport" content="width=device-width, initial-scale=1.0" {}
        title { "Benchmark results" }
        link rel="preconnect" href="https://fonts.googleapis.com" {}
        link rel="preconnect" href="https://fonts.gstatic.com" crossorigin {}
        link href="https://fonts.googleapis.com/css2?family=Roboto+Flex:opsz,wght@8..144,300;8..144,400;8..144,500;8..144,600&display=swap" rel="stylesheet" {}
        style {
          (PreEscaped(include_str!("index.css")))
        }
        script src="https://cdn.plot.ly/plotly-2.26.0.min.js" {}
        script {
          (PreEscaped(format!(r#"const data = JSON.parse("{}");"#, json_escaped)))
        }
        script {
          (PreEscaped(include_str!("index.js")))
        }
      }
      body {
        div.container {
          div.controls id="controls" {
            div.control-group {
              label for="metric" { "Metric" }
              select id="metric" {}
            }
          }
          div.section {
            div id="chart" {}
          }
        }
      }
    }
  }
}

fn main() {
  if let Some(arg) = std::env::args().nth(1) {
    assert_eq!(
      arg, "--suite",
      "usage: benchmark-plotter [--suite samples.jsonl]"
    );
    let path = std::env::args().nth(2).expect("suite JSONL path required");
    render_suite(Path::new(&path)).expect("render benchmark suite");
    return;
  }
  let datasets = load_results();
  println!("Loaded {} datasets", datasets.len());

  let html = generate_html(datasets);

  // Create out directory if it doesn't exist
  fs::create_dir_all("out").unwrap();

  fs::write("out/index.html", html.into_string()).unwrap();

  println!("Generated out/index.html successfully!");
  println!("Open out/index.html in your browser to view the interactive charts.");
}
