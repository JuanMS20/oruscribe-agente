//! Las dos rutas de estado responden el snapshot de `App::state()`.

mod soporte;

use soporte::{AppDePrueba, afirmar_cabeceras, cuerpo_json, levantar};

#[tokio::test]
async fn state_y_auth_state_responden_logged_out() {
    let (base, server) = levantar(AppDePrueba::nuevo().construir()).await;

    let client = reqwest::Client::new();
    for ruta in ["/api/state", "/api/auth/state"] {
        let response = client.get(format!("{base}{ruta}")).send().await.unwrap();
        assert_eq!(response.status(), 200, "{ruta}");
        afirmar_cabeceras(&response);
        let value = cuerpo_json(response).await;
        assert_eq!(value["status"], "logged_out", "{ruta}: {value}");
        assert!(
            value.get("locale").is_some(),
            "{ruta}: falta locale: {value}"
        );
        assert_eq!(value["flow"], false, "{ruta}: {value}");
        assert_eq!(value["retrying"], false, "{ruta}: {value}");
        assert!(
            value.get("me").is_none(),
            "{ruta}: logged-out no debe traer me: {value}"
        );
        assert!(
            value.get("error").is_none(),
            "{ruta}: logged-out no debe traer error: {value}"
        );
    }

    server.abort();
    let _ = server.await;
}
