#![feature(plugin)]
#![plugin(rocket_codegen)]

extern crate forkengine;
#[macro_use] extern crate serde_json;
#[macro_use] extern crate serde_derive;
extern crate rocket;
extern crate rocket_contrib;

use forkengine::{Runtime, RuntimeProduct};
use rocket_contrib::Json;

static RUNTIME_EXECUTION_LIMIT: usize = 500;
static RUNTIME_MEMORY_LIMIT: usize = 32000;

fn product_to_json(product: RuntimeProduct) -> String {
	let mut serialized_snapshots = Vec::new();

	for snapshot in product.snapshots {
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

	json!({
		"snapshots": serialized_snapshots,
		"output": product.output,
		"executions": product.executions,
		"time": product.time
	}).to_string()
}

#[derive(Deserialize)]
struct InterpretationRequest {
	instructions: String,
	input: String
}

#[post("/request_interpretation", data = "<request>")]
fn handle_json_request(request: Json<InterpretationRequest>) -> String {

	let product = Runtime::with_limits(
		request.instructions.clone(), request.input.as_bytes().to_vec(),
		RUNTIME_EXECUTION_LIMIT, RUNTIME_MEMORY_LIMIT
	).run();

	print!("executed {} instructions in {:.2} ms, ", product.executions, (product.time as f64 / 1000000.0) as f64);
	let json = product_to_json(product);
	println!("returned {} bytes", json.len());

	json
}

fn main() {
	rocket::ignite().mount("/", routes![handle_json_request]).launch();
}
