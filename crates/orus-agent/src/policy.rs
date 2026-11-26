//! Política de permisos.

/// Qué puede hacer una herramienta si se corre.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Risk {
    Read,
    Write,
    Exec,
}

impl Risk {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Exec => "exec",
        }
    }

    /// El string del contrato. `None` es el valor desconocido que la política niega.
    pub fn parse(texto: &str) -> Option<Self> {
        match texto {
            "read" => Some(Self::Read),
            "write" => Some(Self::Write),
            "exec" => Some(Self::Exec),
            _ => None,
        }
    }
}

/// El permiso que el usuario le da al agente.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    Autonomous,
    Ask,
    ReadOnly,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Autonomous => "autonomous",
            Self::Ask => "ask",
            Self::ReadOnly => "readonly",
        }
    }

    /// El string del contrato. `None` es el valor desconocido que la política niega.
    pub fn parse(texto: &str) -> Option<Self> {
        match texto {
            "autonomous" => Some(Self::Autonomous),
            "ask" => Some(Self::Ask),
            "readonly" => Some(Self::ReadOnly),
            _ => None,
        }
    }
}

/// La respuesta de la política para una llamada.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Decision {
    Allow,
    AskUser,
    Deny,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::AskUser => "ask_user",
            Self::Deny => "deny",
        }
    }
}

/// Dice si una llamada corre, pide confirmación o se niega.
///
/// `Mode` y `Risk` son strings
/// y un valor fuera de la lista se niega; en Rust el
/// enum no admite esos valores, así que el borde de parseo devuelve `None` y
/// `decide` lo trata como desconocido.
pub fn decide(mode: Option<Mode>, risk: Option<Risk>) -> Decision {
    let Some(risk) = risk else {
        return Decision::Deny;
    };
    let Some(mode) = mode else {
        return Decision::Deny;
    };
    match mode {
        Mode::Autonomous => match risk {
            Risk::Read | Risk::Write | Risk::Exec => Decision::Allow,
        },
        Mode::Ask => match risk {
            Risk::Read => Decision::Allow,
            Risk::Write | Risk::Exec => Decision::AskUser,
        },
        Mode::ReadOnly => match risk {
            Risk::Read => Decision::Allow,
            Risk::Write | Risk::Exec => Decision::Deny,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Los 11 casos.
    #[test]
    fn decide_allow_ask_deny() {
        let casos = [
            (
                "autónomo deja leer",
                Some(Mode::Autonomous),
                Some(Risk::Read),
                Decision::Allow,
            ),
            (
                "autónomo deja escribir",
                Some(Mode::Autonomous),
                Some(Risk::Write),
                Decision::Allow,
            ),
            (
                "autónomo deja ejecutar",
                Some(Mode::Autonomous),
                Some(Risk::Exec),
                Decision::Allow,
            ),
            (
                "pedir deja leer",
                Some(Mode::Ask),
                Some(Risk::Read),
                Decision::Allow,
            ),
            (
                "pedir pregunta al escribir",
                Some(Mode::Ask),
                Some(Risk::Write),
                Decision::AskUser,
            ),
            (
                "pedir pregunta al ejecutar",
                Some(Mode::Ask),
                Some(Risk::Exec),
                Decision::AskUser,
            ),
            (
                "solo lectura deja leer",
                Some(Mode::ReadOnly),
                Some(Risk::Read),
                Decision::Allow,
            ),
            (
                "solo lectura niega escribir",
                Some(Mode::ReadOnly),
                Some(Risk::Write),
                Decision::Deny,
            ),
            (
                "solo lectura niega ejecutar",
                Some(Mode::ReadOnly),
                Some(Risk::Exec),
                Decision::Deny,
            ),
            (
                "modo vacío niega todo",
                None,
                Some(Risk::Read),
                Decision::Deny,
            ),
            (
                "riesgo vacío se niega en autónomo",
                Some(Mode::Autonomous),
                None,
                Decision::Deny,
            ),
        ];

        for (nombre, modo, riesgo, querido) in casos {
            assert_eq!(decide(modo, riesgo), querido, "caso: {nombre}");
        }
    }

    /// Los strings son parte del contrato.
    #[test]
    fn strings_del_contrato() {
        assert_eq!(Risk::Read.as_str(), "read");
        assert_eq!(Risk::Write.as_str(), "write");
        assert_eq!(Risk::Exec.as_str(), "exec");
        assert_eq!(Mode::Autonomous.as_str(), "autonomous");
        assert_eq!(Mode::Ask.as_str(), "ask");
        assert_eq!(Mode::ReadOnly.as_str(), "readonly");
        assert_eq!(Decision::Allow.as_str(), "allow");
        assert_eq!(Decision::AskUser.as_str(), "ask_user");
        assert_eq!(Decision::Deny.as_str(), "deny");
        assert_eq!(Mode::parse("ask"), Some(Mode::Ask));
        assert_eq!(Mode::parse(""), None);
        assert_eq!(Risk::parse("exec"), Some(Risk::Exec));
    }
}
