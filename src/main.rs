#![feature(plugin)]
#![plugin(rocket_codegen)]

extern crate forkengine;
#[macro_use] extern crate serde_json;
#[macro_use] extern crate serde_derive;
extern crate rocket;
extern crate rocket_contrib;

use forkengine::{Runtime, RuntimeSnapshot};
use rocket_contrib::Json;

fn snapshots_to_json(snapshots: Vec<RuntimeSnapshot>) -> String {
	let mut serialized_snapshots = Vec::new();

	for snapshot in snapshots {
		serialized_snapshots.push(
			json!({
				"memory": snapshot.memory,
				"memory_pointer": snapshot.memory_pointer,
				"instruction_pointer": snapshot.instruction_pointer,
				"input_pointer": snapshot.input_pointer,
				"output": snapshot.output,

				"is_error": snapshot.is_error,
				"message": snapshot.message
			})
		);
	}

	json!(serialized_snapshots).to_string()
}

#[derive(Deserialize)]
struct InterpretationRequest {
	instructions: String,
	input: String
}

#[post("/request_interpretation", data = "<request>")]
fn handle_json_request(request: Json<InterpretationRequest>) -> String {
	let snapshots = Runtime::new(request.instructions.clone(), request.input.as_bytes().to_vec()).run().snapshots;
	print!("executed {} instructions, ", snapshots.len());
	let json = snapshots_to_json(snapshots);
	println!("returned {} bytes", json.len());
	json
}

fn main() {
	rocket::ignite().mount("/", routes![handle_json_request]).launch();
}
