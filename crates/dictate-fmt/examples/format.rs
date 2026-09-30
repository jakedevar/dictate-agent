//! Run the default text chain on text and show what each rule did.
//!
//! ```bash
//! cargo run -p dictate-fmt --example format -- "um so the the config is broken"
//! echo "create plan for twenty five percent" | cargo run -p dictate-fmt --example format
//! ```
//!
//! Prints the rules output on stdout and, on stderr, the protected spans and
//! per-stage timings.

use std::io::BufRead;

use dictate_fmt::{FormatContext, TextChain};

fn show(chain: &TextChain, input: &str) {
    let run = chain.run(input, &FormatContext::default());
    println!("{}", run.doc.restore());
    for span in run.doc.spans_in_text_order() {
        eprintln!("  protected {:<13} {:?}", span.kind.as_str(), span.text);
    }
    eprintln!("  {}", run.timings);
}

fn main() {
    let chain = TextChain::default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            show(&chain, &line);
        }
    } else {
        show(&chain, &args.join(" "));
    }
}
