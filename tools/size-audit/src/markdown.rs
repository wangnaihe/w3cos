//! Human-readable size report uploaded next to the JSON artifact.

use crate::attribution::Attribution;
use crate::{CheckedInBaselineReport, Report};

pub fn render(report: &Report) -> String {
    let mut out = String::new();
    out.push_str(&format!("# W3COS size audit — {}\n\n", report.profile));
    out.push_str(&format!("Probe: `{}`\n\n", report.probe));
    if let Some(target) = &report.target {
        out.push_str(&format!("Target: `{target}`\n\n"));
    }
    if !report.features.is_empty() {
        out.push_str(&format!(
            "Enabled features: {}\n\n",
            report.features.join(", ")
        ));
    }
    out.push_str(&format!(
        "Budget: {} bytes. Total: {} bytes. Within budget: {}.\n\n",
        report.budget_bytes, report.total_bytes, report.within_budget
    ));
    out.push_str("## Executable sections\n\n");
    out.push_str("| Field | Bytes |\n| --- | ---: |\n");
    out.push_str(&format!("| text | {} |\n", report.text_bytes));
    out.push_str(&format!(
        "| data (file-backed, including read-only) | {} |\n",
        report.data_bytes
    ));
    out.push_str(&format!(
        "| read-only data | {} |\n",
        report.readonly_data_bytes
    ));
    out.push_str(&format!("| bss | {} |\n", report.bss_bytes));
    out.push_str(&format!(
        "| other object sections | {} |\n",
        report.other_object_bytes
    ));
    out.push_str(&format!(
        "| gzip -9 download proxy | {} |\n",
        report.gzip_bytes
    ));
    if let Some(compressed) = report.compressed_bytes {
        out.push_str(&format!(
            "| external compressed artifact | {compressed} |\n"
        ));
    }
    out.push_str(&format!(
        "| embedded resources | {} |\n\n",
        report.embedded_bytes
    ));

    out.push_str("## Embedded resources\n\n");
    if report.embedded_resources.is_empty() {
        out.push_str(
            "No font, image, or listed asset bytes were found in the measured components.\n\n",
        );
    } else {
        out.push_str("| Kind | Bytes | Offset | Path |\n| --- | ---: | ---: | --- |\n");
        for resource in &report.embedded_resources {
            out.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                resource.kind,
                resource.bytes,
                resource.offset,
                resource.path.as_deref().unwrap_or("")
            ));
        }
        out.push('\n');
    }

    out.push_str("## Crate attribution\n\n");
    if report.attribution.symbols_present {
        write_attribution(&mut out, &report.attribution);
    } else {
        out.push_str(
            "No symbol table was available. Pass `--symbols` pointing at the unstripped probe.\n\n",
        );
    }

    if let Some(baseline) = &report.checked_in_baseline {
        write_checked_in(&mut out, baseline);
    }
    if let Some(baseline) = &report.baseline {
        out.push_str("## Same-runner comparison\n\n");
        out.push_str(&format!(
            "Baseline bytes: {}. Change: {} bytes ({:.4}%). Limit: {:.4}%. Within threshold: {}.\n\n",
            baseline.bytes,
            baseline.change_bytes,
            baseline.change_percent,
            baseline.max_regression_percent,
            baseline.within_threshold
        ));
    }
    out
}

fn write_attribution(out: &mut String, attribution: &Attribution) {
    out.push_str(&format!(
        "Symbol table accounts for {} bytes across {} symbols. This is not the file size.\n\n",
        attribution.symbol_bytes, attribution.symbol_count
    ));
    out.push_str("### Features\n\n");
    out.push_str("| Feature | Symbol bytes |\n| --- | ---: |\n");
    for feature in &attribution.features {
        out.push_str(&format!("| {} | {} |\n", feature.feature, feature.bytes));
    }
    out.push_str("\nFeature rows are symbol-table bytes for crates that belong to that feature. They can overlap and are not separate link measurements. A zero means those crates were absent from this probe.\n\n");
    out.push_str("### Crates\n\n");
    out.push_str("| Crate | Symbol bytes | Symbols |\n| --- | ---: | ---: |\n");
    for item in attribution.crates.iter().take(30) {
        out.push_str(&format!(
            "| {} | {} | {} |\n",
            item.name, item.bytes, item.symbols
        ));
    }
    out.push_str("\n### Top symbols\n\n");
    out.push_str("| Bytes | Crate | Symbol |\n| ---: | --- | --- |\n");
    for symbol in &attribution.top_symbols {
        out.push_str(&format!(
            "| {} | {} | `{}` |\n",
            symbol.bytes,
            symbol.crate_name,
            symbol.name.replace('|', "\\|")
        ));
    }
    out.push('\n');
}

fn write_checked_in(out: &mut String, baseline: &CheckedInBaselineReport) {
    out.push_str("## Checked-in baseline\n\n");
    if !baseline.compared {
        out.push_str(&format!(
            "Not compared ({}); this target has no checked-in row. Cross-architecture numbers are not a failure.\n\n",
            baseline.reason
        ));
        return;
    }
    out.push_str(&format!(
        "Baseline: {} bytes. Change: {} bytes ({:.4}%). Fails above {:.4}% or {} bytes. Within threshold: {}.\n\n",
        baseline.bytes.unwrap_or(0),
        baseline.change_bytes,
        baseline.change_percent,
        baseline.max_regression_percent,
        baseline.max_regression_bytes,
        baseline.within_threshold
    ));
    if !baseline.note.is_empty() {
        out.push_str(&format!("Baseline note: {}\n\n", baseline.note));
    }
}
