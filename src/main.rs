use clap::Parser;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[derive(Parser, Debug)]
#[command(author, version, about = "Queries MTR (Hong Kong subway) point-to-point journey times.")]
struct Args {
    /// Departure station (fuzzy matching, case-insensitive)
    #[arg(value_name = "FROM")]
    from: Option<String>,

    /// Destination station (fuzzy matching, case-insensitive)
    #[arg(value_name = "TO")]
    to: Option<String>,

    /// Re-scrapes piliapp and updates cache
    #[arg(long)]
    refresh: bool,

    /// Prints full N×N table as TSV to stdout
    #[arg(long)]
    matrix: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
struct Cache {
    stations: Vec<String>,
    // matrix[from_station][to_station] = minutes
    matrix: HashMap<String, HashMap<String, u32>>,
}

fn get_cache_path() -> PathBuf {
    let mut path = home::home_dir().expect("Could not find home directory");
    path.push(".local/share/poros/cache.json");
    path
}

fn check_agent_browser() -> Result<(), String> {
    Command::new("agent-browser")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| "agent-browser not found — install it first".to_string())?;
    Ok(())
}

fn run_agent_command(args: &[&str]) -> Result<String, String> {
    let output = Command::new("agent-browser")
        .args(args)
        .output()
        .map_err(|e| format!("Failed to run agent-browser: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "agent-browser failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn scrape() -> Result<Cache, String> {
    check_agent_browser()?;

    println!("Scraping piliapp.com (takes ~5 min)...");
    
    run_agent_command(&["open", "https://www.piliapp.com/hongkong-mtr/"])?;
    
    // Switch to Journey Time mode
    run_agent_command(&[
        "eval",
        "document.querySelectorAll('option')[0].selected=true; document.querySelectorAll('select')[0].dispatchEvent(new Event('change',{bubbles:true})); 'ok'"
    ])?;

    let mut cache = Cache::default();
    let mut station_names = Vec::new();

    for i in 1..=120 {
        let id = format!("t{}", i);
        let click_res = run_agent_command(&[
            "eval",
            &format!("var el=document.getElementById('{}'); if(el) {{ el.click(); 'ok' }} else 'skip'", id)
        ])?;

        if click_res.contains("skip") {
            continue;
        }

        print!("\rProgress: station {}/120...", i);
        io::stdout().flush().unwrap();

        run_agent_command(&["wait", "300"])?;

        let times_json = run_agent_command(&[
            "eval",
            "var r={}; document.querySelectorAll('span[id^=\"t\"]').forEach(function(s){ var name=s.textContent.trim(); var sib=s.nextSibling; var t=sib?sib.textContent.trim():''; if(t&&!isNaN(parseInt(t)))r[name]=parseInt(t); }); JSON.stringify(r)"
        ])?;

        // Handle possible quotes around the JSON string from agent-browser eval
        let times_json_clean = if (times_json.starts_with('"') && times_json.ends_with('"')) || (times_json.starts_with('\'') && times_json.ends_with('\'')) {
            &times_json[1..times_json.len()-1]
        } else {
            &times_json
        };
        
        // agent-browser might escape quotes in the JSON string
        let times_json_unescaped = times_json_clean.replace("\\\"", "\"");

        let times: HashMap<String, u32> = serde_json::from_str(&times_json_unescaped)
            .map_err(|e| format!("Failed to parse times JSON for {}: {}. JSON: {}", id, e, times_json_unescaped))?;

        let active_name = run_agent_command(&[
            "eval",
            &format!("document.getElementById('{}').textContent.trim()", id)
        ])?;
        
        let active_name = if (active_name.starts_with('"') && active_name.ends_with('"')) || (active_name.starts_with('\'') && active_name.ends_with('\'')) {
            active_name[1..active_name.len()-1].to_string()
        } else {
            active_name
        };

        if !station_names.contains(&active_name) {
            station_names.push(active_name.clone());
        }

        cache.matrix.insert(active_name, times);
    }

    println!("\nScraping complete.");
    run_agent_command(&["close"])?;

    station_names.sort();
    cache.stations = station_names;

    Ok(cache)
}

fn save_cache(cache: &Cache) -> Result<(), String> {
    let path = get_cache_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create cache directory: {}", e))?;
    }
    let json = serde_json::to_string_pretty(cache).map_err(|e| format!("Failed to serialize cache: {}", e))?;
    fs::write(path, json).map_err(|e| format!("Failed to write cache file: {}", e))?;
    Ok(())
}

fn load_cache() -> Result<Cache, String> {
    let path = get_cache_path();
    if !path.exists() {
        return Err("Cache file missing".to_string());
    }
    let json = fs::read_to_string(path).map_err(|e| format!("Failed to read cache file: {}", e))?;
    let cache: Cache = serde_json::from_str(&json).map_err(|e| format!("Failed to parse cache: {}", e))?;
    Ok(cache)
}

fn fuzzy_match<'a>(query: &str, stations: &'a [String]) -> Result<&'a String, String> {
    let query_lower = query.to_lowercase();
    
    // Exact match first
    if let Some(s) = stations.iter().find(|s| s.to_lowercase() == query_lower) {
        return Ok(s);
    }
    
    let matches: Vec<&String> = stations
        .iter()
        .filter(|s| s.to_lowercase().contains(&query_lower))
        .collect();

    match matches.len() {
        0 => {
            let mut sorted_stations = stations.to_vec();
            sorted_stations.sort();
            Err(format!(
                "Station '{}' not found. Available stations:\n{}",
                query,
                sorted_stations.join(", ")
            ))
        },
        1 => Ok(matches[0]),
        _ => Err(format!(
            "Ambiguous station name '{}'. Matches: {}",
            query,
            matches
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn main() {
    let args = Args::parse();

    let cache_res = load_cache();
    
    let cache = if args.refresh {
        match scrape() {
            Ok(new_cache) => {
                save_cache(&new_cache).expect("Failed to save cache");
                new_cache
            }
            Err(e) => {
                eprintln!("Error scraping: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        match cache_res {
            Ok(c) => c,
            Err(_) => {
                println!("Cache empty, scraping piliapp.com (takes ~5 min)...");
                match scrape() {
                    Ok(new_cache) => {
                        save_cache(&new_cache).expect("Failed to save cache");
                        new_cache
                    }
                    Err(e) => {
                        eprintln!("Error scraping: {}", e);
                        std::process::exit(1);
                    }
                }
            }
        }
    };

    if args.refresh {
        return;
    }

    if args.matrix {
        print!("\t");
        println!("{}", cache.stations.join("\t"));
        for from in &cache.stations {
            print!("{}\t", from);
            let row = cache.matrix.get(from).cloned().unwrap_or_default();
            let values: Vec<String> = cache
                .stations
                .iter()
                .map(|to| row.get(to).map(|t| t.to_string()).unwrap_or_else(|| "".to_string()))
                .collect();
            println!("{}", values.join("\t"));
        }
        return;
    }

    if let (Some(from_query), Some(to_query)) = (args.from, args.to) {
        let from_station = match fuzzy_match(&from_query, &cache.stations) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        };

        let to_station = match fuzzy_match(&to_query, &cache.stations) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        };

        if let Some(row) = cache.matrix.get(from_station) {
            if let Some(time) = row.get(to_station) {
                println!("{} min", time);
            } else {
                eprintln!("No journey time found between {} and {}", from_station, to_station);
                std::process::exit(1);
            }
        } else {
            eprintln!("No journey time data for {}", from_station);
            std::process::exit(1);
        }
    } else if !args.refresh && !args.matrix {
        eprintln!("Usage: poros <FROM> <TO> or poros --refresh or poros --matrix");
        std::process::exit(1);
    }
}
