#![feature(plugin, const_max_value)]
#![plugin(rocket_codegen)]

extern crate forkengine;
#[macro_use] extern crate serde_json;
#[macro_use] extern crate serde_derive;
extern crate rocket;
extern crate rocket_contrib;
extern crate syncbox;

use std::io::Cursor;

use rocket::Outcome;
use rocket::http::{Status, ContentType};
use rocket::request::{self, Request, State, FromRequest};
use rocket::response::{Responder, Response};
use rocket_contrib::Json;
use syncbox::atomic::{AtomicU64, Ordering};
use forkengine::{Runtime, RuntimeProduct};


static ADMIN_EXECUTION_LIMIT: usize = usize::max_value();
static ADMIN_MEMORY_LIMIT: usize = usize::max_value();

static DEVELOPER_EXECUTION_LIMIT: usize = 10000;
static DEVELOPER_MEMORY_LIMIT: usize = 128000;

static DEFAULT_EXECUTION_LIMIT: usize = 500;
static DEFAULT_MEMORY_LIMIT: usize = 32000;

#[derive(Debug)]
enum AccessLevel {
	Administrator,
	Developer,
	Unauthenticated,
	UnknownLevel
}

fn get_privilege_byte(api_key: &str) -> Option<u8> {
	match api_key.trim() {
		"redacted" => return Some(0), // global admin key
		"redacted" => return Some(1), // global developer key
		_ => return None
	}
}

impl<'a, 'r> FromRequest<'a, 'r> for AccessLevel {
	type Error = ();

	fn from_request(request: &'a Request<'r>) -> request::Outcome<AccessLevel, ()> {
		let keys: Vec<_> = request.headers().get("X-Authentication").collect();
		if keys.len() > 1 { // no more than one auth key per request
			return Outcome::Failure((Status::BadRequest, ())); // "too many API keys!"
		}

		if let Some(api_key) = keys.get(0) {
			if let Some(privilege) = get_privilege_byte(api_key) {
				return Outcome::Success(AccessLevel::from_privilege_byte(privilege));
			} else {
				return Outcome::Failure((Status::Unauthorized, ())); // "API key not recognized!"
			}
		}

		return Outcome::Success(AccessLevel::Unauthenticated);
	}
}

impl AccessLevel {

	fn from_privilege_byte(value: u8) -> AccessLevel {
		match value {
			0 => AccessLevel::Administrator,
			1 => AccessLevel::Developer,
			_ => AccessLevel::UnknownLevel
		}
	}

	// (execution_limit, memory_limit)
	fn get_runtime_limits(&self) -> (usize, usize) {
		match self {
			&AccessLevel::Administrator => (ADMIN_EXECUTION_LIMIT, ADMIN_MEMORY_LIMIT),
			&AccessLevel::Developer => (DEVELOPER_EXECUTION_LIMIT, DEVELOPER_MEMORY_LIMIT),
			_ => (DEFAULT_EXECUTION_LIMIT, DEFAULT_MEMORY_LIMIT)
		}
	}

}


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


// JSON type for an API request to interpret some brainfuck instructions and input
#[derive(Deserialize)]
struct InterpretationRequest {
	instructions: String,
	input: String
}

struct JSONResponse(String);

impl<'r> Responder<'r> for JSONResponse {
	fn respond_to(self, _: &Request) -> Result<Response<'static>, Status> {
		Response::build()
			.header(ContentType::JSON) // set the appropriate content-type
			.sized_body(Cursor::new(self.0)) // sized body containing this type's internal String
			.ok()
	}
}

#[post("/api/request_interpretation", data = "<request>")]
fn handle_json_request(request: Json<InterpretationRequest>, access_level: AccessLevel, server_counts: State<ServerCounts>) -> JSONResponse {

	let (execution_limit, memory_limit) = access_level.get_runtime_limits();
	let product = Runtime::with_limits(
		request.instructions.clone(), request.input.as_bytes().to_vec(),
		execution_limit, memory_limit
	).run();

	server_counts.instructions.fetch_add(product.executions as u64, Ordering::Relaxed);
	server_counts.runtime_ns.fetch_add(product.time as u64, Ordering::Relaxed);
	println!("executed {} instructions in {:.2} ms", product.executions, (product.time as f64 / 1000000.0) as f64);

	let json = product_to_json(product);

	server_counts.requests_fufilled.fetch_add(1, Ordering::Relaxed);
	server_counts.bytes_returned.fetch_add(json.len() as u64, Ordering::Relaxed);
	println!("returned {} bytes for {:?}", json.len(), access_level);

	JSONResponse(json)
}

#[get("/api")]
fn handle_status_request(server_counts: State<ServerCounts>) -> String {
	format!(
		"
	brainfork API status

	fufilled {} requests
	executed {} instructions
	engine runtime: {:.2} ms
	returned {} chars


	rendered this page {} times
		",
		server_counts.requests_fufilled.load(Ordering::Relaxed),
		server_counts.instructions.load(Ordering::Relaxed),
		(server_counts.runtime_ns.load(Ordering::Relaxed) as f64) / 1000000.0,
		server_counts.bytes_returned.load(Ordering::Relaxed),
		server_counts.status_hits.fetch_add(1, Ordering::Relaxed) + 1
	)
}


struct ServerCounts {
	requests_fufilled: AtomicU64,
	instructions: AtomicU64,
	runtime_ns: AtomicU64,
	bytes_returned: AtomicU64,
	status_hits: AtomicU64
}

impl ServerCounts {
	fn new() -> ServerCounts {
		ServerCounts {
			requests_fufilled: AtomicU64::new(0),
			instructions: AtomicU64::new(0),
			runtime_ns: AtomicU64::new(0),
			bytes_returned: AtomicU64::new(0),
			status_hits: AtomicU64::new(0)
		}
	}
}

fn main() {
	rocket::ignite().manage(ServerCounts::new()).mount("/", routes![handle_json_request, handle_status_request]).launch();
}
