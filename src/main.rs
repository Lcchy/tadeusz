use std::io;

use anyhow::Result;
use jack::{Client, ClientOptions};

fn main() -> Result<()> {
    let (jclient, client_status) = Client::new("tadeusz_jack", ClientOptions::NO_START_SERVER)?;

    // Register ports. They will be used in a callback that will be
    // called when new data is available.
    let in_a = jclient
        .register_port("tadeusz_in_l", jack::AudioIn::default())
        .unwrap();
    let in_b = jclient
        .register_port("tadeusz_in_r", jack::AudioIn::default())
        .unwrap();
    let mut out_a = jclient
        .register_port("tadeusz_out_l", jack::AudioOut::default())
        .unwrap();
    let mut out_b = jclient
        .register_port("tadeusz_out_r", jack::AudioOut::default())
        .unwrap();
    let process_callback = move |_: &jack::Client, ps: &jack::ProcessScope| -> jack::Control {
        let out_a_p = out_a.as_mut_slice(ps);
        let out_b_p = out_b.as_mut_slice(ps);
        let in_a_p = in_a.as_slice(ps);
        let in_b_p = in_b.as_slice(ps);
        out_a_p.clone_from_slice(in_a_p);
        out_b_p.clone_from_slice(in_b_p);
        jack::Control::Continue
    };
    let process = jack::ClosureProcessHandler::new(process_callback);

    // Activate the client, which starts the processing.
    let active_client = jclient.activate_async((), process).unwrap();

    // Wait for user input to quit
    println!("Press enter/return to quit...");
    let mut user_input = String::new();
    io::stdin().read_line(&mut user_input).ok();

    active_client.deactivate().unwrap();

    Ok(())
}
