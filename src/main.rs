#![feature(plugin, const_max_value)]
#![plugin(rocket_codegen)]

extern crate forkengine;
#[macro_use] extern crate serde_json;
#[macro_use] extern crate serde_derive;
extern crate rocket;
extern crate rocket_contrib;
extern crate syncbox;
extern crate r2d2;
extern crate r2d2_sqlite;
extern crate uuid;

use std::env;
use std::io::Cursor;

use forkengine::{Runtime, RuntimeProduct};

use rocket::Outcome;
use rocket::http::{Status, ContentType};
use rocket::response::{Responder, Response};
use rocket::request::{self, Request, State, FromRequest};
use rocket_contrib::Json;
use syncbox::atomic::{AtomicU64, Ordering};

use r2d2_sqlite::SqliteConnectionManager;


// AccessLevel runtime limits (execution_limit, memory_limit)
static ADMIN_RUNTIME_LIMITS: (usize, usize) 		= (usize::max_value(), usize::max_value());
static DEVELOPER_RUNTIME_LIMITS: (usize, usize) 	= (10000, 128000);
static BASIC_RUNTIME_LIMITS: (usize, usize) 		= (5000, 64000);
static DEFAULT_RUNTIME_LIMITS: (usize, usize) 		= (500, 32000);

#[derive(Debug, PartialEq)]
enum AccessLevel {
	Administrator = 0,
	Developer = 1,
	Basic = 2,
	Unauthenticated = -1
}

impl<'a, 'r> FromRequest<'a, 'r> for AccessLevel {
	type Error = ();

	fn from_request(request: &'a Request<'r>) -> request::Outcome<AccessLevel, ()> {
		let keys: Vec<_> = request.headers().get("X-Authentication").collect(); // collect all provided X-Authentication headers
		if keys.len() > 1 { // if more than 1 API key provided
			return Outcome::Failure((Status::BadRequest, ())); // "too many API keys!"
		}

		// there is now either 0 or 1 API key in 'keys'

		if let Some(api_key) = keys.get(0) { // if there is one, evaluate it

			let pool = request.guard::<State<SQLitePool>>()?; // get the connection pool from the server
			match pool.get() {
				Ok(conn) => {
					if let Some(access_level) = get_access_level(&conn, &api_key) {
						return Outcome::Success(access_level);
					} else {
						return Outcome::Failure((Status::Unauthorized, ())); // "API key not recognized!"
					}
				},
				Err(_) => return Outcome::Failure((Status::ServiceUnavailable, ()))
			}

		} else {
			// otherwise, return a neutral default access level
			return Outcome::Success(AccessLevel::Unauthenticated);
		}

	}
}

impl AccessLevel {

	fn from_access_byte(value: u8) -> Option<AccessLevel> {
		match value {
			0 => Some(AccessLevel::Administrator),
			1 => Some(AccessLevel::Developer),
			2 => Some(AccessLevel::Basic),
			_ => None
		}
	}

	// (execution_limit, memory_limit)
	fn get_runtime_limits(&self) -> (usize, usize) {
		match self {
			&AccessLevel::Administrator => ADMIN_RUNTIME_LIMITS,
			&AccessLevel::Developer => DEVELOPER_RUNTIME_LIMITS,
			&AccessLevel::Basic => BASIC_RUNTIME_LIMITS,
			_ => DEFAULT_RUNTIME_LIMITS
		}
	}

}


fn product_to_response(product: RuntimeProduct) -> JSONResponse {
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

	JSONResponse(
		json!({
			"snapshots": serialized_snapshots,
			"output": product.output,
			"executions": product.executions,
			"time": product.time
		}).to_string()
	)
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


// request to interpret some brainfuck instructions and input
#[derive(Deserialize)]
struct InterpretationRequest {
	instructions: String,
	input: String
}

#[post("/api/request_interpretation", data = "<request>")]
fn handle_interpretation(request: Json<InterpretationRequest>, access_level: AccessLevel, server_counts: State<ServerCounts>) -> JSONResponse {

	let (execution_limit, memory_limit) = access_level.get_runtime_limits();
	let product = Runtime::with_limits(
		request.instructions.clone(), request.input.as_bytes().to_vec(),
		execution_limit, memory_limit
	).run();

	server_counts.instructions.fetch_add(product.executions as u64, Ordering::Relaxed);
	server_counts.runtime_ns.fetch_add(product.time as u64, Ordering::Relaxed);
	println!("executed {} instructions in {:.2} ms", product.executions, (product.time as f64 / 1000000.0) as f64);

	let response = product_to_response(product);

	server_counts.requests_fufilled.fetch_add(1, Ordering::Relaxed);
	server_counts.bytes_returned.fetch_add(response.0.len() as u64, Ordering::Relaxed);
	println!("returned {} bytes for {:?}", response.0.len(), access_level);

	response
}

#[get("/api")]
fn handle_api_status(server_counts: State<ServerCounts>) -> String {
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

// request to generate a new API key (currently by default a Developer)
#[derive(Deserialize)]
struct AuthorizationRequest {
	access_level: String,
	label: String
}

#[post("/api/new_authorization", data = "<request>")]
fn handle_new_authorization(request: Json<AuthorizationRequest>, access_level: AccessLevel, database: Database) -> JSONResponse {
	if access_level != AccessLevel::Administrator {
		return JSONResponse(
			json!({
				"error_message": "you are not authorized to request this feature" // return failure TODO: HTTP code
			}).to_string()
		);
	}

	let access_requested;
	match request.access_level.as_ref() {
		"developer" => access_requested = AccessLevel::Developer,
		"basic" => access_requested = AccessLevel::Basic,
		_ => return JSONResponse(
			json!({
				"error_message": "unknown 'access_level', expected: {'developer', 'basic'}" // return failure TODO: HTTP code
			}).to_string()
		)
	}

	let json;
	if let Some(new_key) = insert_api_key(&database.0, access_requested, &request.label) { // if success
		json = json!({
			"key": new_key // return the key
		})
	} else {
		json = json!({
			"error_message": "failed to insert key into database" // return failure TODO: HTTP code
		})
	}

	JSONResponse(json.to_string())
}

#[get("/api/limits")]
fn handle_limits(access_level: AccessLevel) -> JSONResponse {
	let (execution_limit, memory_limit) = access_level.get_runtime_limits();
	JSONResponse(
		json!({
			"access_level": format!("{:?}", access_level),
			"execution_limit": execution_limit,
			"memory_limit": memory_limit
		}).to_string()
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


static SQL_INIT_KEYS: &'static str = "
	CREATE TABLE IF NOT EXISTS authorizations (
		api_key CHAR(36) PRIMARY KEY NOT NULL,
		access_level SMALLINT NOT NULL,
		label VARCHAR(40) NOT NULL
	)
";

static SQL_INSERT_KEY: &'static str = "
	INSERT INTO authorizations (api_key, access_level, label)
	VALUES (?1, ?2, ?3)
";

static SQL_SELECT_KEY_ACCESS: &'static str = "
	SELECT access_level FROM authorizations WHERE api_key = ?
	LIMIT 1
";

type SQLitePool = r2d2::Pool<SqliteConnectionManager>;
type SQLiteConnection = r2d2::PooledConnection<r2d2_sqlite::SqliteConnectionManager>;

struct Database(SQLiteConnection);

impl<'a, 'r> FromRequest<'a, 'r> for Database {
	type Error = ();

	fn from_request(request: &'a Request<'r>) -> request::Outcome<Database, ()> {
		let pool = request.guard::<State<SQLitePool>>()?;
		match pool.get() {
			Ok(conn) => Outcome::Success(Database(conn)),
			Err(_) => Outcome::Failure((Status::ServiceUnavailable, ()))
		}
	}
}

fn init_db() -> SQLitePool {
	let pool = r2d2::Pool::new(
		r2d2::Config::default(),
		SqliteConnectionManager::new(
			env::var("BRAINFORK_DB").ok()
				.expect("failed to load BRAINFORK_DB environment variable!")
		)
	).expect("failed to initialize SQLite connection pool!");

	pool.get().unwrap().execute(SQL_INIT_KEYS, &[]).unwrap();

	pool
}

fn insert_api_key(conn: &SQLiteConnection, access_level: AccessLevel, label: &str) -> Option<String> {
	let key = uuid::Uuid::new_v4().hyphenated().to_string();
	let result = conn.execute(
		SQL_INSERT_KEY,
		&[
			&key,
			&(access_level as u8),
			&label
		]
	);

	match result {
		Ok(_) => return Some(key),
		Err(err) => {
			println!("{:?}", err);
			return None;
		}
	}
}

fn get_access_level(conn: &SQLiteConnection, api_key: &str) -> Option<AccessLevel> {
	conn.query_row(
		SQL_SELECT_KEY_ACCESS,
		&[&api_key],
		|result| {
			AccessLevel::from_access_byte(result.get(0)).unwrap()
		}
	).ok()
}


fn main() {
	rocket::ignite()
		.manage(ServerCounts::new())
		.manage(init_db())
		.mount("/", routes![handle_interpretation, handle_api_status, handle_new_authorization, handle_limits])
		.launch();
}
