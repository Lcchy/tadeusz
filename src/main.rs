use anyhow::{bail, Result};
use jack::{Client, ClientOptions};
use rosc::OscMessage;
use std::f32::consts::PI;
use std::fs::File;
use std::path::Path;
use std::str::FromStr;
use std::{
    cmp::min,
    io,
    net::UdpSocket,
    sync::{Arc, RwLock},
    thread,
};
use strum::EnumString;

/// Should be enough,See https://osc-dev.create.ucsb.narkive.com/TyotlluU/osc-udp-packet-sizes-for-interoperability
/// and https://www.music.mcgill.ca/~gary/306/week9/osc.html
// const OSC_BUFFER_LEN: usize = 4096;
const OSC_BUFFER_LEN: usize = rosc::decoder::MTU;
const OSC_PORT: &str = "34254";
const SAMPLE_BUFFER_MAX_LEN: usize = 10 * 48000; // 10s
const XFADE_LEN: usize = 150; // samples, ~2ms at 48khz

// Write: -, Read: osc+jack
struct SampleBuffer {
    len: usize,
    /// Of length SAMPLE_BUFFER_MAX_LEN
    r_buffer: Vec<f32>,
    l_buffer: Vec<f32>,
    /// Constant power xfade env for grain loop smoothing.
    /// Of same length as samplefbuffer
    /// Sin for XFADE_LEN, 1 after
    xfade_in: Vec<f32>,
    /// Cos for XFADE_LEN, 0 after
    /// Of same length as samplefbuffer
    xfade_out: Vec<f32>,
}

#[derive(Clone, PartialEq, EnumString, Debug)]
enum GrainStatus {
    Off,
    On,
    XFade,
}

#[derive(Clone)]
struct GrainParams {
    // On or off
    status: GrainStatus,
    /// Mark the params as recently changed
    updated: bool,
    len: usize,
    /// Sample index to start from
    start: usize,
    /// Cycles per second, Hz
    speed: f32,
}

#[derive(Clone)]
struct Params {
    // Write: jack process, Read: -
    grain_head: usize,
    // Write: osc process, Read: Jack process
    grain: Arc<RwLock<GrainParams>>,
}

fn main() -> Result<()> {
    // Set up jack ports
    let (jclient, _) = Client::new("tadeusz_jack", ClientOptions::NO_START_SERVER)?;
    // let in_l_port = jclient
    //     .register_port("tadeusz_in_l", jack::AudioIn::default())
    //     .unwrap();
    // let in_r_port = jclient
    //     .register_port("tadeusz_in_r", jack::AudioIn::default())
    //     .unwrap();
    let mut out_l_port = jclient
        .register_port("tadeusz_out_l", jack::AudioOut::default())
        .unwrap();
    let mut out_r_port = jclient
        .register_port("tadeusz_out_r", jack::AudioOut::default())
        .unwrap();

    //Load audio file into sample buffer
    let mut inp_file = File::open(Path::new("Plate1.wav"))?;
    let (header, data) = wav::read(&mut inp_file)?;
    let sample_rate = jclient.sample_rate();
    if sample_rate != header.sampling_rate as usize {
        println!(
            "Sample rate of file ({}) does not match the one from Jack ({})",
            header.sampling_rate, sample_rate
        );
        // Err(Error::msg(
        //     "Sample rate of file does not match the one from Jack",
        // ))?;
    }

    // TODO wheres the stereo??
    let bits: Vec<f32> = data
        .as_sixteen()
        .unwrap()
        .iter()
        .map(|&x| (x as f32) / 32768f32)
        .collect();

    // Compute cross fading env
    let buffer_len = min(SAMPLE_BUFFER_MAX_LEN, bits.len());
    let mut xfade_in: Vec<f32> = (0..XFADE_LEN)
        .map(|i| (PI * i as f32 / 2. * XFADE_LEN as f32).sin())
        .collect::<Vec<f32>>();
    let mut xfade_out: Vec<f32> = (0..XFADE_LEN)
        .map(|i| (PI * i as f32 / 2. * XFADE_LEN as f32).cos())
        .collect();
    xfade_in.resize(buffer_len, 1.);
    xfade_out.resize(buffer_len, 0.);

    let buffer = SampleBuffer {
        len: buffer_len,
        r_buffer: bits[0..buffer_len].to_vec(),
        l_buffer: bits[0..buffer_len].to_vec(),
        xfade_in,
        xfade_out,
    };

    // Create the shared parameters instance
    let grain_params_arc = Arc::new(RwLock::new(GrainParams {
        status: GrainStatus::XFade,
        updated: false,
        start: 0,
        len: buffer.len,
        speed: 1.,
    }));
    let params_arc = Params {
        grain_head: 0,
        grain: grain_params_arc,
    };

    // Define the Jack process (to refactor)
    let mut params_ref = params_arc.clone();
    let b_arc = Arc::new(buffer);
    let b_ref = b_arc.clone();
    let jack_process = move |_: &jack::Client, ps: &jack::ProcessScope| -> jack::Control {
        let out_l_buff = out_l_port.as_mut_slice(ps);
        let out_r_buff = out_r_port.as_mut_slice(ps);
        // let mut in_a_p = in_a.as_slice(ps).to_owned();
        // let mut in_b_p = in_b.as_slice(ps).to_owned();

        let grain_params_read = params_ref.grain.read().unwrap();

        // Relying on buffer L R being same length
        let out_buff_len = out_l_buff.len();
        let grain_len = grain_params_read.len;
        for i in 0..out_buff_len {
            //TODO case when grain_len < XFADE_OUT
            let grain_pos = params_ref.grain_head + i;
            let buffer_pos = grain_params_read.start + grain_pos % grain_len;
            // Stays at -1 until (end_of_grain - XFADE) is reached, wrapped
            let xfade_pos =
                (grain_pos.saturating_sub(grain_len - XFADE_LEN)) % grain_len + b_ref.len - 1;
            let grain_status = &grain_params_read.status;

            match grain_status {
                GrainStatus::XFade => {
                    out_l_buff[i] =
                        // Fade out from the end of the grain
                        // TODO could store last played samples in a buffer of xfade_size to be used for fadeout? 
                        // TODO cut to 0 crossinng
                        b_ref.xfade_out[grain_pos % grain_len]
                            * b_ref.l_buffer[(buffer_pos + grain_len) % b_ref.len] +
                        // Present grain
                        b_ref.xfade_in[grain_pos % grain_len]
                            * b_ref.l_buffer[buffer_pos % b_ref.len]
                            * b_ref.xfade_out[(xfade_pos as usize + 1) % b_ref.len]
                        // Fade in of next grain
                        + b_ref.xfade_in[xfade_pos as usize % b_ref.len] * b_ref.l_buffer[(b_ref.len + buffer_pos - grain_len) % b_ref.len];

                    // Same for R
                    out_r_buff[i] = b_ref.xfade_out[grain_pos % grain_len]
                        * b_ref.r_buffer[(buffer_pos + grain_len) % b_ref.len]
                        + b_ref.xfade_in[grain_pos % grain_len]
                            * b_ref.r_buffer[buffer_pos % b_ref.len]
                            * b_ref.xfade_out[(xfade_pos as usize + 1) % b_ref.len]
                        + b_ref.xfade_in[xfade_pos as usize % b_ref.len]
                            * b_ref.r_buffer[(b_ref.len + buffer_pos - grain_len) % b_ref.len];
                }
                GrainStatus::On => {
                    out_l_buff[i] = b_ref.l_buffer[buffer_pos % b_ref.len];

                    out_r_buff[i] = b_ref.r_buffer[buffer_pos % b_ref.len];
                }
                GrainStatus::Off => {
                    out_l_buff.fill(0.);
                    out_r_buff.fill(0.);
                }
            }
        }
        params_ref.grain_head = (params_ref.grain_head + out_buff_len) % grain_len;

        jack::Control::Continue
    };

    // Start the Jack thread
    let process = jack::ClosureProcessHandler::new(jack_process);
    let active_client = jclient.activate_async((), process).unwrap();

    // Start the OSC listening thread
    let udp_socket = UdpSocket::bind(format!("127.0.0.1:{}", OSC_PORT))?;
    let osc_process = osc_process_closure(udp_socket, params_arc, b_arc);
    let osc_handler = thread::spawn(osc_process);

    // Wait for user input to quit
    println!("Press enter/return to quit...");
    let mut user_input = String::new();
    io::stdin().read_line(&mut user_input).ok();
    active_client.deactivate().unwrap();
    let osc_res = osc_handler.join();
    println!("OSC shutdown: {:?}", osc_res);

    Ok(())
}

fn osc_handling(osc_msg: &OscMessage, params: &Params, buffer: &SampleBuffer) -> Result<()> {
    match osc_msg.addr.as_str() {
        "/tadeusz/status" => {
            let status = osc_msg.args[0]
                .to_owned()
                .string()
                .ok_or_else(|| anyhow::format_err!("OSC status arg was not recognized."))?;
            let mut grain_params_mut = params.grain.write().unwrap();
            grain_params_mut.status = GrainStatus::from_str(&status)?;
            println!("Grain Status set to {:?}", grain_params_mut.status);
        }
        "/tadeusz/params" => {
            let mut grain_params_mut = params.grain.write().unwrap();
            let start = osc_msg.args[0]
                .to_owned()
                .int()
                .ok_or_else(|| anyhow::format_err!("OSC start arg was not recognized."))?;
            let len = osc_msg.args[1]
                .to_owned()
                .int()
                .ok_or_else(|| anyhow::format_err!("OSC len arg was not recognized."))?;

            if len > XFADE_LEN as i32 {
                grain_params_mut.start = min(start as usize, buffer.len);
                grain_params_mut.len = min(len as usize, buffer.len);
                println!("Grain start set to {:?}", grain_params_mut.start);
                println!("Grain len set to {:?}", len);
            } else {
                println!("OSC len message argument cannot be less than XFADE.");
            }
        }
        _ => bail!("OSC routing was not recognized"),
    }
    Ok(())
}

/// Returns a closure that runs the main osc receiving loop
fn osc_process_closure(
    udp_socket: UdpSocket,
    params_ref: Params,
    buffer: Arc<SampleBuffer>,
) -> impl FnOnce() -> Result<()> {
    move || {
        let mut rec_buffer = [0; OSC_BUFFER_LEN];
        loop {
            match udp_socket.recv(&mut rec_buffer) {
                Ok(received) => {
                    let (_, packet) =
                        if let Ok(v) = rosc::decoder::decode_udp(&rec_buffer[..received]) {
                            v
                        } else {
                            println!("OSC message could not be decoded.");
                            continue;
                        };
                    match packet {
                        rosc::OscPacket::Message(msg) => {
                            println!("Received osc msg {:?}", msg);
                            let r = osc_handling(&msg, &params_ref, &buffer);
                            if let Err(e) = r {
                                println!("OSC message hnadling failed with: {:?}", e);
                            }
                        }
                        rosc::OscPacket::Bundle(_) => unimplemented!(),
                    }
                }
                Err(e) => println!("recv function failed: {:?}", e),
            }
        }
    }
}
