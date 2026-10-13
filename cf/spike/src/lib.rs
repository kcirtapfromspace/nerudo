//! Platform spike: KV-backed Durable Object storage, alarms, and the Artifacts binding from Rust.
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use worker::*;

#[durable_object]
pub struct Trunk {
    state: State,
}

impl DurableObject for Trunk {
    fn new(state: State, _env: Env) -> Self {
        Self { state }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        let s = self.state.storage();
        match req.path().as_str() {
            "/do/incr" => {
                let n: i64 = s.get("n").await?.unwrap_or(0) + 1;
                s.put("n", n).await?;
                Response::ok(format!("n={n}"))
            }
            "/do/alarm" => {
                s.set_alarm(1000).await?;
                Response::ok("alarm set for +1s")
            }
            _ => {
                let fired: Option<String> = s.get("alarm_fired").await?;
                Response::ok(format!("alarm_fired={fired:?}"))
            }
        }
    }

    async fn alarm(&self) -> Result<Response> {
        self.state.storage().put("alarm_fired", Date::now().to_string()).await?;
        Response::ok("ok")
    }
}

/// The Artifacts binding has no workers-rs wrapper; reach it as a plain JS object.
struct Binding(JsValue);
impl EnvBinding for Binding {
    const TYPE_NAME: &'static str = "";
    fn get(val: JsValue) -> Result<Self> {
        Ok(Binding(val))
    }
}
impl JsCast for Binding {
    fn instanceof(_: &JsValue) -> bool { true }
    fn unchecked_from_js(val: JsValue) -> Self { Binding(val) }
    fn unchecked_from_js_ref(val: &JsValue) -> &Self { unsafe { &*(val as *const JsValue as *const Self) } }
}
impl AsRef<JsValue> for Binding { fn as_ref(&self) -> &JsValue { &self.0 } }
impl From<Binding> for JsValue { fn from(b: Binding) -> Self { b.0 } }

async fn call(obj: &JsValue, method: &str, args: &[JsValue]) -> Result<JsValue> {
    let f: js_sys::Function = js_sys::Reflect::get(obj, &method.into())?.dyn_into()?;
    let arr = js_sys::Array::new();
    for a in args { arr.push(a); }
    let p = f.apply(obj, &arr)?;
    Ok(JsFuture::from(js_sys::Promise::from(p)).await?)
}

#[event(fetch)]
async fn main(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let path = req.path();
    if path.starts_with("/do/") {
        let stub = env.durable_object("TRUNK")?.id_from_name("spike")?.get_stub()?;
        return stub.fetch_with_request(req).await;
    }
    if path == "/artifacts/create" {
        let ns = env.get_binding::<Binding>("ARTIFACTS")?;
        let name = req.url()?.query_pairs().find(|(k, _)| k == "name").map(|(_, v)| v.to_string())
            .unwrap_or("spike-repo".into());
        return match call(&ns.0, "create", &[name.into()]).await {
            Ok(v) => {
                let json = js_sys::JSON::stringify(&v)?.as_string().unwrap_or_default();
                // Never echo the token back in a response body.
                let redacted: serde_json::Value = serde_json::from_str(&json).unwrap_or_default();
                Response::ok(format!("created name={} remote={}", redacted["name"], redacted["remote"]))
            }
            Err(e) => Response::error(format!("artifacts create failed: {e:?}"), 502),
        };
    }
    Response::ok("nerudo spike: /do/incr /do/alarm /do/check /artifacts/create")
}
