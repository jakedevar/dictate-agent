//! Golden corpus for the default deterministic chain.
//!
//! Every input is **invented** (the repository is public): shaped like what
//! Jake dictates — prompts for coding agents, chat, email, notes — but not
//! taken from any real transcript. Each pair states the intended output of the
//! default `[format]` configuration; a change to a rule that moves any of
//! these is a behavior change and must be reviewed as one.
//!
//! Properties checked over the whole corpus:
//! - idempotence: `rules(rules(x)) == rules(x)`;
//! - protected spans survive byte-for-byte (paths only through the built-in
//!   `.cloud/` → `.claude/` fix) and in order;
//! - the output verifies against its own document (what the LLM guard runs).

use dictate_fmt::text::rules::corrected_path;
use dictate_fmt::{FormatConfig, FormatContext, RulesConfig, SpanKind, TextChain};

/// Prompts for coding agents (Claude Code / Codex in a terminal).
const CODING: &[(&str, &str)] = &[
    (
        "research code base for how the auth middleware handles expired tokens",
        "/research_codebase for how the auth middleware handles expired tokens.",
    ),
    (
        "um, can you look at src/main.rs and figure out why the build fails",
        "Can you look at src/main.rs and figure out why the build fails.",
    ),
    (
        "run /research_codebase on the payment service",
        "Run /research_codebase on the payment service.",
    ),
    (
        "create plan for adding retry logic to the uploader",
        "/create_plan for adding retry logic to the uploader.",
    ),
    ("okay now implement plan", "Okay now /implement_plan"),
    (
        "the the function get_user_by_id returns none when it should throw",
        "The function get_user_by_id returns none when it should throw.",
    ),
    (
        "set the timeout to thirty seconds and retry five times",
        "Set the timeout to 30 seconds and retry five times.",
    ),
    (
        "bump the version to two point one point three in Cargo.toml",
        "Bump the version to 2.1.3 in Cargo.toml",
    ),
    (
        "check ~/.cloud/settings.json and make sure the hooks are registered",
        "Check ~/.claude/settings.json and make sure the hooks are registered.",
    ),
    (
        "ask cloud to review the diff before we merge",
        "Ask Claude to review the diff before we merge.",
    ),
    (
        "uh the tests in crates/dictate-fmt/tests are flaky can you look",
        "The tests in crates/dictate-fmt/tests are flaky can you look.",
    ),
    (
        "what does the useEffect hook do in App.tsx",
        "What does the useEffect hook do in App.tsx",
    ),
    (
        "rename getUserName to fetchUserName everywhere",
        "Rename getUserName to fetchUserName everywhere.",
    ),
    (
        "I I think the bug is in the the parser",
        "I think the bug is in the parser.",
    ),
    (
        "run cargo test --workspace and paste the failures",
        "Run cargo test --workspace and paste the failures.",
    ),
    (
        "the endpoint is https://api.example.com/v1/users so hit that",
        "The endpoint is https://api.example.com/v1/users so hit that.",
    ),
    (
        "use port eight thousand for the dev server",
        "Use port 8000 for the dev server.",
    ),
    (
        "limit the batch size to one hundred and twenty eight",
        "Limit the batch size to 128.",
    ),
    (
        "we're at ninety five percent coverage let's get to one hundred percent",
        "We're at 95% coverage let's get to 100%",
    ),
    (
        "then validate plan and write a summary",
        "Then /validate_plan and write a summary.",
    ),
    (
        "hmm, the migration takes like five minutes which is way too long",
        "The migration takes like 5 minutes which is way too long.",
    ),
    (
        "can you add a flag called dry_run that defaults to false",
        "Can you add a flag called dry_run that defaults to false.",
    ),
    (
        "look at the .env file and tell me which keys are missing",
        "Look at the .env file and tell me which keys are missing.",
    ),
    (
        "the error says connection refused on localhost:5432",
        "The error says connection refused on localhost:5432.",
    ),
    ("create handoff", "/create_handoff"),
    (
        "please, um, don't touch the scripts directory",
        "Please, don't touch the scripts directory.",
    ),
    (
        "wh- what's the difference between Arc and Rc here",
        "What's the difference between Arc and Rc here.",
    ),
    (
        "st-stop using unwrap in the daemon code",
        "Stop using unwrap in the daemon code.",
    ),
    (
        "the regex should match both foo_bar and fooBar",
        "The regex should match both foo_bar and fooBar",
    ),
    (
        "move the constants into config.rs and re-export them",
        "Move the constants into config.rs and re-export them.",
    ),
    (
        "so the the webhook fires twice um because we register it twice",
        "So the webhook fires twice because we register it twice.",
    ),
    (
        "make sure /clear runs before /compact",
        "Make sure /clear runs before /compact",
    ),
    (
        "write it to /tmp/dictate/out.log and tail it",
        "Write it to /tmp/dictate/out.log and tail it.",
    ),
    (
        "increase the retry count from three to ten",
        "Increase the retry count from three to 10.",
    ),
    (
        "can you check whether @jake approved the PR in #infra",
        "Can you check whether @jake approved the PR in #infra",
    ),
    (
        "the vector should hold twenty five thousand entries max",
        "The vector should hold 25,000 entries max.",
    ),
    (
        "Um so basically the cache is never invalidated",
        "So basically the cache is never invalidated.",
    ),
    (
        "it takes about two point five gigabytes of VRAM",
        "It takes about 2.5 gigabytes of VRAM.",
    ),
    (
        "open the README.md and fix the typo in the install section",
        "Open the README.md and fix the typo in the install section.",
    ),
    ("the the the build is green now", "The build is green now."),
    (
        "we should use version three of the API not version two",
        "We should use version 3 of the API not version 2.",
    ),
    (
        "delete the old node_modules folder first",
        "Delete the old node_modules folder first.",
    ),
    (
        "the docs are at docs.rs/tokio",
        "The docs are at docs.rs/tokio",
    ),
    (
        "can you explain what std::mem::take does here",
        "Can you explain what std::mem::take does here.",
    ),
    (
        "wrap the call in `tokio::time::timeout` please",
        "Wrap the call in `tokio::time::timeout` please.",
    ),
    (
        "the ci job at .github/workflows/ci.yml is failing",
        "The ci job at .github/workflows/ci.yml is failing.",
    ),
    (
        "set RUST_LOG to debug and rerun",
        "Set RUST_LOG to debug and rerun.",
    ),
    (
        "then create hand off so the next session can pick it up",
        "Then /create_handoff so the next session can pick it up.",
    ),
    ("for i in range ten print i", "For i in range 10 print i."),
    (
        "add um error handling for the the network call",
        "Add error handling for the network call.",
    ),
    (
        "log the p99 latency every sixty seconds",
        "Log the p99 latency every 60 seconds.",
    ),
    (
        "set max tokens to four thousand ninety six",
        "Set max tokens to 4096.",
    ),
    (
        "the build takes like four minutes on CI",
        "The build takes like 4 minutes on CI.",
    ),
    (
        "Um, research code base for the retry logic",
        "/research_codebase for the retry logic.",
    ),
    ("summarize the diff /no_think", "Summarize the diff"),
    (
        "can you run the tests. Thank you.",
        "Can you run the tests.",
    ),
    (
        "edit ./.clod/commands/review.md so it asks for tests",
        "Edit ./.claude/commands/review.md so it asks for tests.",
    ),
    (
        "i tested it and i think the fix is right",
        "I tested it and I think the fix is right.",
    ),
    (
        "the function is called parse_config not parseConfig",
        "The function is called parse_config not parseConfig",
    ),
    (
        "compare HashMap and BTreeMap for this use case",
        "Compare HashMap and BTreeMap for this use case.",
    ),
];

/// Chat messages.
const CHAT: &[(&str, &str)] = &[
    (
        "hey um are you free for lunch tomorrow",
        "Hey are you free for lunch tomorrow.",
    ),
    ("lol that's hilarious", "Lol that's hilarious"),
    (
        "i'm running like ten minutes late sorry",
        "I'm running like 10 minutes late sorry.",
    ),
    (
        "can we move the meeting to three thirty pm",
        "Can we move the meeting to 3:30 PM.",
    ),
    ("yeah yeah sounds good", "Yeah yeah sounds good."),
    (
        "no no no that's not what i meant",
        "No no no that's not what I meant.",
    ),
    ("thanks so much", "Thanks so much"),
    (
        "Thank you for the quick turnaround on this",
        "Thank you for the quick turnaround on this.",
    ),
    ("ok see you at five", "Ok see you at five."),
    (
        "did you see the email from sarah about the offsite",
        "Did you see the email from sarah about the offsite.",
    ),
    (
        "uh-oh the deploy failed again",
        "Uh-oh the deploy failed again.",
    ),
    (
        "grab me a coffee if you're going out",
        "Grab me a coffee if you're going out.",
    ),
    ("the the wifi is down again", "The wifi is down again."),
    (
        "it costs twenty dollars and fifty cents total",
        "It costs $20.50 total.",
    ),
    (
        "meet me at the cafe on fifth street",
        "Meet me at the cafe on fifth street.",
    ),
    (
        "I can't make it hmm maybe next week",
        "I can't make it maybe next week.",
    ),
    ("haha yeah exactly", "Haha yeah exactly"),
    (
        "wait what time is the thing",
        "Wait what time is the thing.",
    ),
    ("one sec", "One sec"),
    (
        "send me the link when you get a chance",
        "Send me the link when you get a chance.",
    ),
    (
        "happy birthday hope you have a great one",
        "Happy birthday hope you have a great one.",
    ),
    ("we won by twenty one points", "We won by 21 points."),
    ("the game starts at seven pm", "The game starts at 7 PM."),
    ("brb", "Brb"),
    ("that's so so cool", "That's so so cool."),
    ("i i don't know honestly", "I don't know honestly."),
    (
        "Er, what was the address again?",
        "What was the address again?",
    ),
    ("lmk if you need anything", "Lmk if you need anything."),
    ("the kids are eight and ten", "The kids are eight and 10."),
    (
        "i'll be there in like twenty mins",
        "I'll be there in like 20 mins.",
    ),
    ("Mm-hmm, sounds good to me", "Mm-hmm, sounds good to me."),
    (
        "can you pick up milk eggs and bread",
        "Can you pick up milk eggs and bread.",
    ),
    ("running five minutes behind", "Running 5 minutes behind."),
    (
        "oh nice congrats on the new job",
        "Oh nice congrats on the new job.",
    ),
    ("you and i should talk", "You and I should talk."),
    ("hmm", ""),
    (
        "we're sitting in row twelve seat four",
        "We're sitting in row 12 seat four.",
    ),
    (
        "the umbrella is by the door",
        "The umbrella is by the door.",
    ),
    (
        "that that is the question honestly",
        "That that is the question honestly.",
    ),
    (
        "send it to #random and ping @sam",
        "Send it to #random and ping @sam",
    ),
];

/// Email.
const EMAIL: &[(&str, &str)] = &[
    (
        "hi team, um, quick update on the migration",
        "Hi team, quick update on the migration.",
    ),
    (
        "please send the invoice to billing@example.com by friday",
        "Please send the invoice to billing@example.com by friday.",
    ),
    (
        "the contract is worth two million dollars over three years",
        "The contract is worth $2 million over three years.",
    ),
    (
        "we saw a fifteen percent increase in signups last quarter",
        "We saw a 15% increase in signups last quarter.",
    ),
    (
        "thanks for your patience. thank you.",
        "Thanks for your patience.",
    ),
    ("i wanted to thank you.", "I wanted to thank you."),
    (
        "looking forward to hearing from you",
        "Looking forward to hearing from you.",
    ),
    (
        "the meeting is at ten thirty am in room four",
        "The meeting is at 10:30 AM in room four.",
    ),
    (
        "let me know if you have any questions",
        "Let me know if you have any questions.",
    ),
    (
        "our budget for this is about five thousand dollars",
        "Our budget for this is about $5,000.",
    ),
    (
        "please review the doc at https://docs.example.com/document/d/abc123 before monday",
        "Please review the doc at https://docs.example.com/document/d/abc123 before monday.",
    ),
    ("sorry for the late reply", "Sorry for the late reply."),
    (
        "the the deadline moved to march fifteenth",
        "The deadline moved to march fifteenth.",
    ),
    (
        "can we schedule a call for next tuesday at two pm",
        "Can we schedule a call for next tuesday at 2 PM.",
    ),
    ("Best regards", "Best regards"),
    (
        "the total came to one hundred and forty nine dollars",
        "The total came to $149.",
    ),
    (
        "I've cc'd jane.doe@example.com on this thread",
        "I've cc'd jane.doe@example.com on this thread.",
    ),
    (
        "per our conversation the rollout starts on the first",
        "Per our conversation the rollout starts on the first.",
    ),
    (
        "we need sign off from legal, finance, and ops",
        "We need sign off from legal, finance, and ops.",
    ),
    (
        "the price went up by three point five percent",
        "The price went up by 3.5%",
    ),
    (
        "hope you had a great weekend",
        "Hope you had a great weekend.",
    ),
    (
        "our office hours are nine am to five pm",
        "Our office hours are 9 AM to 5 PM.",
    ),
    (
        "the file is too large, it's about forty megabytes",
        "The file is too large, it's about 40 megabytes.",
    ),
    (
        "unfortunately we can't support that until version four",
        "Unfortunately we can't support that until version 4.",
    ),
    (
        "um, just following up on my last email",
        "Just following up on my last email.",
    ),
    (
        "Dear Ms. Patel, thank you for the update",
        "Dear Ms. Patel, thank you for the update.",
    ),
    (
        "the attached report covers q three, e.g. revenue and churn",
        "The attached report covers q three, e.g. revenue and churn.",
    ),
    (
        "i think we should, uh, revisit the pricing",
        "I think we should, revisit the pricing.",
    ),
    (
        "the renewal is one hundred twenty thousand dollars per year",
        "The renewal is $120,000 per year.",
    ),
    (
        "we'll discuss it on the twenty third",
        "We'll discuss it on the twenty third.",
    ),
];

/// Notes and reminders.
const NOTES: &[(&str, &str)] = &[
    ("todo buy groceries", "Todo buy groceries"),
    (
        "idea: a cli that summarizes git logs",
        "Idea: a cli that summarizes git logs.",
    ),
    (
        "remember to call mom on sunday",
        "Remember to call mom on sunday.",
    ),
    (
        "the recipe needs two cups of flour and one egg",
        "The recipe needs two cups of flour and one egg.",
    ),
    ("run five k on saturday", "Run five k on saturday."),
    (
        "the dentist appointment is at eleven fifteen am",
        "The dentist appointment is at 11:15 AM.",
    ),
    (
        "call the plumber about the leak",
        "Call the plumber about the leak.",
    ),
    (
        "the garden needs about three hours of sun",
        "The garden needs about 3 hours of sun.",
    ),
    (
        "pick up the dry cleaning before six",
        "Pick up the dry cleaning before six.",
    ),
    ("gym at six thirty am tomorrow", "Gym at 6:30 AM tomorrow."),
    (
        "the car needs an oil change in five hundred miles",
        "The car needs an oil change in 500 miles.",
    ),
    (
        "note to self uh don't forget the charger",
        "Note to self don't forget the charger.",
    ),
    (
        "the password hint is the name of my first dog",
        "The password hint is the name of my first dog.",
    ),
    (
        "temperature was ninety eight point six this morning",
        "Temperature was 98.6 this morning.",
    ),
    (
        "save twenty percent of every paycheck",
        "Save 20% of every paycheck.",
    ),
    (
        "the lease ends in twenty twenty seven",
        "The lease ends in twenty twenty seven.",
    ),
    (
        "project kickoff is on the tenth",
        "Project kickoff is on the tenth.",
    ),
    ("Um.", ""),
    ("the the", "The"),
    ("Thank you.", ""),
    (
        "okay so the plan is to ship on friday. thank you.",
        "Okay so the plan is to ship on friday.",
    ),
    (
        "books to read dune and neuromancer",
        "Books to read dune and neuromancer.",
    ),
    (
        "water the plants every three days",
        "Water the plants every three days.",
    ),
    (
        "the flight is at five forty five am on the second",
        "The flight is at 5:45 AM on the second.",
    ),
    (
        "one of the best talks was about CRDTs",
        "One of the best talks was about CRDTs.",
    ),
    (
        "two people asked about the one on one format",
        "Two people asked about the one on one format.",
    ),
    (
        "hmm, maybe try the blue one instead",
        "Maybe try the blue one instead.",
    ),
    (
        "pi is roughly three point one four one five nine",
        "Pi is roughly 3.14159.",
    ),
];

/// Whisper already did the work: clean input must come back unchanged.
const ALREADY_CLEAN: &[(&str, &str)] = &[
    (
        "We need to fix the login flow before Friday.",
        "We need to fix the login flow before Friday.",
    ),
    ("Can you check the logs?", "Can you check the logs?"),
    (
        "The build failed with 3 errors and 12 warnings.",
        "The build failed with 3 errors and 12 warnings.",
    ),
    (
        "It costs $5,000, which is 20% over budget.",
        "It costs $5,000, which is 20% over budget.",
    ),
    (
        "Meet me at 5:30 p.m. near the station.",
        "Meet me at 5:30 p.m. near the station.",
    ),
    (
        "Run /research_codebase, then /create_plan.",
        "Run /research_codebase, then /create_plan.",
    ),
    (
        "Open ~/.claude/settings.json and check the hooks.",
        "Open ~/.claude/settings.json and check the hooks.",
    ),
    (
        "Version 2.1.3 fixed it, e.g. the race in the parser.",
        "Version 2.1.3 fixed it, e.g. the race in the parser.",
    ),
    ("Thank you for the review!", "Thank you for the review!"),
    ("Wait... are you sure?", "Wait... are you sure?"),
    ("Line one.\nLine two.", "Line one.\nLine two."),
    (
        "Claude, can you refactor this?",
        "Claude, can you refactor this?",
    ),
];

/// Routing triggers must survive formatting (the router runs on this output).
const ROUTES: &[(&str, &str)] = &[
    ("timer ten minutes", "Timer 10 minutes"),
    ("um timer ten minutes", "Timer 10 minutes"),
    (
        "timer twenty five minutes for the tea",
        "Timer 25 minutes for the tea.",
    ),
    (
        "easy what is the capital of France",
        "Easy what is the capital of France.",
    ),
    (
        "edit: make this more formal",
        "Edit: make this more formal.",
    ),
    ("hard, explain monads", "Hard, explain monads"),
];

fn all() -> impl Iterator<Item = &'static (&'static str, &'static str)> {
    CODING
        .iter()
        .chain(CHAT)
        .chain(EMAIL)
        .chain(NOTES)
        .chain(ALREADY_CLEAN)
        .chain(ROUTES)
}

fn rules(text: &str) -> String {
    TextChain::default().format(text, &FormatContext::default())
}

#[test]
fn the_corpus_is_big_enough_to_mean_something() {
    assert!(all().count() >= 150, "corpus has {} pairs", all().count());
}

#[test]
fn every_pair_formats_as_expected() {
    let mut failures = Vec::new();
    for (input, want) in all() {
        let got = rules(input);
        if got != *want {
            failures.push(format!(
                "  input: {input:?}\n   want: {want:?}\n    got: {got:?}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} pairs differ:\n{}",
        failures.len(),
        all().count(),
        failures.join("\n")
    );
}

#[test]
fn formatting_is_idempotent() {
    for (input, _) in all() {
        let once = rules(input);
        let twice = rules(&once);
        assert_eq!(twice, once, "not idempotent for {input:?}");
    }
}

#[test]
fn protected_spans_survive_byte_for_byte_and_in_order() {
    for (input, _) in all() {
        let run = TextChain::default().run(input, &FormatContext::default());
        let output = run.doc.restore();
        // What `protect` marked (after the scrub, which deliberately removes
        // `/no_think`), independently of every later stage.
        let protected_only = TextChain::standard(&FormatConfig {
            enabled: true,
            rules: RulesConfig {
                hallucination_scrub: true,
                builtin_corrections: false,
                fillers: false,
                stutters: false,
                numbers: false,
                casing: false,
                spacing: false,
                terminal_punctuation: false,
                spoken_punctuation: false,
                spoken_line_breaks: false,
            },
            ..FormatConfig::default()
        })
        .run(input, &FormatContext::default());
        // Each is in the final output unchanged (paths only through the
        // built-in `.cloud/` → `.claude/` fix), in order.
        let mut pos = 0;
        for span in protected_only.doc.spans_in_text_order() {
            let expected = if span.kind == SpanKind::Path {
                corrected_path(&span.text)
            } else {
                span.text.clone()
            };
            let at = output[pos..]
                .find(&expected)
                .unwrap_or_else(|| panic!("{expected:?} lost from {input:?} → {output:?}"));
            pos += at + expected.len();
        }
        // And the output passes the guard the LLM output must pass.
        assert_eq!(
            run.doc.verify_output(&output),
            Ok(()),
            "self-verification failed for {input:?}"
        );
    }
}

#[test]
fn slash_commands_survive_every_stage() {
    for input in [
        "research code base",
        "um research codebase for the the auth flow",
        "run /research_codebase now",
        "Research code base. Thank you.",
        "/research_codebase",
        "then /research_codebase twenty five times",
    ] {
        let out = rules(input);
        assert!(
            out.contains("/research_codebase"),
            "{input:?} → {out:?} lost the slash command"
        );
        assert!(!out.contains("/Research"), "{out:?}");
    }
}
