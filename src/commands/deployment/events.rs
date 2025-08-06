use clap::{Command, Arg, ArgMatches};
use cli_table::{print_stdout, Table, WithTitle};
use serde::Deserialize;

use crate::config::config::{Config, load_auth_config};

#[derive(Deserialize, Debug, Clone)]
struct EventItem {
    id: String,
    deployment_id: String,
    timestamp: String,
    level: String,
    message: String,
    component: String,
    reason: Option<String>,
}

#[derive(Table)]
struct EventTableItem {
    #[table(title = "Time")]
    timestamp: String,
    #[table(title = "Level")]
    level: String,
    #[table(title = "Component")]
    component: String,
    #[table(title = "Reason")]
    reason: String,
    #[table(title = "Message")]
    message: String,
}

pub(crate) fn command_config() -> Command {
    Command::new("events")
        .about("Get events for a deployment")
        .arg(
            Arg::new("deployment_id")
                .required(true)
                .help("The deployment ID to get events for")
        )
        .arg(
            Arg::new("level")
                .long("level")
                .short('l')
                .help("Filter events by level (info, warning, error)")
                .value_parser(["info", "warning", "error"])
        )
        .arg(
            Arg::new("limit")
                .long("limit")
                .help("Limit number of events returned")
                .value_parser(clap::value_parser!(u32))
                .default_value("50")
        )
        .arg(
            Arg::new("follow")
                .long("follow")
                .short('f')
                .help("Follow events in real-time (like tail -f)")
                .action(clap::ArgAction::SetTrue)
        )
}

pub(crate) fn execute(sub_matches: &ArgMatches, mut config: Config) {
    let deployment_id = sub_matches.get_one::<String>("deployment_id").unwrap();
    let level = sub_matches.get_one::<String>("level");
    let limit = sub_matches.get_one::<u32>("limit").unwrap();
    let follow = sub_matches.get_flag("follow");
    
    if follow {
        follow_events(deployment_id, level, *limit, &mut config);
    } else {
        match fetch_events(deployment_id, level, *limit, &mut config) {
            Ok(events) => {
                if events.is_empty() {
                    println!("No events found for deployment {}", deployment_id);
                } else {
                    display_events(&events);
                }
            }
            Err(e) => {
                eprintln!("Error: {}", e);
            }
        }
    }
}

fn fetch_events(deployment_id: &str, level: Option<&String>, limit: u32, config: &mut Config) -> Result<Vec<EventItem>, String> {
    let mut url = format!("{}/deployments/{}/events?limit={}", 
                         config.get_api_url(), deployment_id, limit);
    
    if let Some(level) = level {
        url.push_str(&format!("&level={}", level));
    }


    let auth_config = load_auth_config(config.name.clone());

    let response = ureq::get(&url)
        .header("Authorization", &format!("Bearer {}", auth_config.token))
        .header("Content-Type", "application/json")
        .call();

    match response {
        Ok(mut response) => {
            let response_text = response.body_mut().read_to_string()
                .map_err(|e| format!("Failed to read response: {}", e))?;

            let events: Vec<EventItem> = serde_json::from_str(&response_text)
                .map_err(|e| format!("Failed to parse response as JSON: {}", e))?;

            Ok(events)
        }
        Err(e) if e.to_string().contains("401") => {
            Err("Authentication failed. Please login again with 'ring login'".to_string())
        }
        Err(e) if e.to_string().contains("404") => {
            Err(format!("Deployment '{}' not found", deployment_id))
        }
        Err(e) => {
            Err(format!("Failed to fetch events: {}", e))
        }
    }
}

fn follow_events(deployment_id: &str, level: Option<&String>, limit: u32, config: &mut Config) {
    println!("Following events for deployment {} (Press Ctrl+C to stop)...", deployment_id);
    
    let mut last_seen_id: Option<String> = None;
    let mut all_events: Vec<EventItem> = Vec::new();
    
    // Show initial events and store them
    let initial_events = fetch_events(deployment_id, level, limit, config);
    if let Ok(events) = initial_events {
        if !events.is_empty() {
            // Reverse the events to show oldest first (tail -f style)
            all_events = events.clone();
            all_events.reverse();
            display_events(&all_events);
            // Take the ID of the most recent event (first in original DESC sorted list)
            last_seen_id = events.first().map(|e| e.id.clone());
        }
    }
    
    // Then follow new events
    loop {

        std::thread::sleep(std::time::Duration::from_secs(2));
        
        match fetch_events(deployment_id, level, limit, config) {
            Ok(events) => {
                let new_events = filter_new_events(&events, &last_seen_id);
                
                if !new_events.is_empty() {
                    // Clear screen and show updated table with new events
                    print!("\x1B[2J\x1B[H"); // Clear screen and move cursor to top
                    println!("Following events for deployment {} (Press Ctrl+C to stop)...", deployment_id);
                    
                    // Add new events to the end (like tail -f behavior)
                    for new_event in new_events.iter().rev() {
                        all_events.push(new_event.clone());
                    }
                    
                    // Keep only the limit number of events (remove old ones from the beginning)
                    if all_events.len() > limit as usize {
                        let excess = all_events.len() - limit as usize;
                        all_events.drain(0..excess);
                    }
                    
                    // Display updated table
                    display_events(&all_events);
                    
                    // Update with the ID of the most recent event
                    last_seen_id = new_events.first().map(|e| e.id.clone());
                }
                
            }
            Err(e) => {
                eprintln!("Error fetching events: {}", e);
            }
        }
    }
}

fn filter_new_events(events: &[EventItem], last_seen_id: &Option<String>) -> Vec<EventItem> {
    if let Some(last_id) = last_seen_id {
        // Find the position of the last seen event
        let mut new_events = Vec::new();
        
        // Events are sorted by timestamp DESC, so we collect until we find the last seen ID
        for event in events {
            if event.id == *last_id {
                break;
            }
            new_events.push(event.clone());
        }
        
        new_events
    } else {
        Vec::new() // No new events if we don't have a reference point
    }
}

fn display_events(events: &[EventItem]) {
    let table_items: Vec<EventTableItem> = events
        .iter()
        .map(|event| EventTableItem {
            timestamp: format_timestamp(&event.timestamp),
            level: event.level.clone(),
            component: event.component.clone(),
            reason: event.reason.clone().unwrap_or_else(|| "-".to_string()),
            message: event.message.clone(),
        })
        .collect();

    print_stdout(table_items.with_title()).expect("Failed to print table");
}

fn display_new_events(events: &[EventItem]) {
    // Style "tail -f" - one line per event
    for event in events {
        let timestamp = format_timestamp(&event.timestamp);
        let level_color = match event.level.as_str() {
            "error" => "\x1b[31m", // Red
            "warning" => "\x1b[33m", // Yellow
            "info" => "\x1b[32m", // Green
            _ => "\x1b[0m", // Default
        };
        
        println!("{}[{}]\x1b[0m {} {}: {}", 
                level_color,
                event.level.to_uppercase(),
                timestamp,
                event.component,
                event.message);
    }
}

fn format_timestamp(timestamp: &str) -> String {
    // Parse ISO timestamp and format it with full date and time
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(timestamp) {
        parsed.format("%Y-%m-%d %H:%M:%S").to_string()
    } else {
        // Fallback: just show the timestamp as-is
        timestamp.to_string()
    }
}