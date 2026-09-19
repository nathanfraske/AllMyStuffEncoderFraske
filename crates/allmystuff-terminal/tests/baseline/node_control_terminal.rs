        // ---- terminal plane ----------------------------------------------
        "term_send" => {
            let route_id: String = try_arg!(arg(a, "route_id"));
            let event: TermEvent = try_arg!(arg(a, "event"));
            json_result(mesh.term_send(route_id, event).await)
        }
        "term_watch" => {
            let route_id: String = try_arg!(arg(a, "route_id"));
            DispatchOut::Json(json!(mesh.term_watch(&route_id)))
        }
        "term_poll" => {
            let route_id: String = try_arg!(arg(a, "route_id"));
            DispatchOut::Bytes(mesh.term_poll(&route_id))
        }
        "term_unwatch" => {
            let route_id: String = try_arg!(arg(a, "route_id"));
            let token: u64 = try_arg!(arg(a, "token"));
            mesh.term_unwatch(&route_id, token);
            DispatchOut::Json(Value::Null)
        }
        "terminal_sessions" => {
            let node: String = try_arg!(arg(a, "node"));
            json_result(mesh.request_terminal_sessions(node).await)
        }
