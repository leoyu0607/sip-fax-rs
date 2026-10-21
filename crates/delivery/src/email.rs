//! E-mail delivery over SMTP (lettre, rustls only).
//!
//! Failure classes: SMTP 4xx, connection errors, timeouts and TLS handshake errors are transient;
//! SMTP 5xx (bad recipient, authentication failure, message rejected), invalid addresses and
//! missing source files are permanent.

use crate::config::{EmailConfig, Format, Security};
use crate::error::DeliveryError;
use crate::source;
use crate::template::{Mode, Template, sanitize_name};
use chrono::FixedOffset;
use lettre::message::header::ContentType;
use lettre::message::{Attachment, Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use std::path::Path;
use std::time::Duration;
use store::FaxRecord;

/// Build the message for `rec` (also used by tests to inspect the result without sending).
pub async fn build_message(
    cfg: &EmailConfig,
    rec: &FaxRecord,
    data_dir: &Path,
    offset: FixedOffset,
) -> Result<Message, DeliveryError> {
    let perm = |what: &str, e: String| DeliveryError::permanent(format!("{what}: {e}"));
    let render = |src: &str| -> Result<String, DeliveryError> {
        let t = Template::parse(src).map_err(|e| perm("template", e))?;
        Ok(t.render(rec, offset, Mode::Text))
    };
    let subject = render(&cfg.subject)?.replace('\n', " ");
    let body = render(&cfg.body)?;
    let stem = match &cfg.attachment_name {
        Some(t) => {
            let t = Template::parse(t).map_err(|e| perm("attachment_name", e))?;
            t.render(rec, offset, Mode::FileName)
        }
        None => format!("fax-{}", rec.id),
    };
    let stem = sanitize_name(stem.strip_suffix(".pdf").unwrap_or(&stem));

    let pdf = source::read(data_dir, rec, Format::Pdf).await?;
    let tiff = if cfg.attach_tiff {
        Some(source::read(data_dir, rec, Format::Tiff).await?)
    } else {
        None
    };

    let from: Mailbox = cfg
        .from
        .parse()
        .map_err(|e| perm("from address", format!("{e}")))?;
    let mut b = Message::builder().from(from).subject(subject);
    for t in &cfg.to {
        let m: Mailbox = t
            .parse()
            .map_err(|e| perm("recipient", format!("{t}: {e}")))?;
        b = b.to(m);
    }
    let mut mp = MultiPart::mixed().singlepart(SinglePart::plain(body));
    mp = mp.singlepart(Attachment::new(format!("{stem}.pdf")).body(
        pdf,
        ContentType::parse("application/pdf").expect("static content type"),
    ));
    if let Some(t) = tiff {
        mp = mp.singlepart(Attachment::new(format!("{stem}.tif")).body(
            t,
            ContentType::parse("image/tiff").expect("static content type"),
        ));
    }
    b.multipart(mp)
        .map_err(|e| perm("building message", e.to_string()))
}

fn transport(cfg: &EmailConfig) -> Result<AsyncSmtpTransport<Tokio1Executor>, DeliveryError> {
    let tls = |insecure: bool| -> Result<TlsParameters, DeliveryError> {
        TlsParameters::builder(cfg.host.clone())
            .dangerous_accept_invalid_certs(insecure)
            .dangerous_accept_invalid_hostnames(insecure)
            .build_rustls()
            .map_err(|e| DeliveryError::permanent(format!("TLS setup: {e}")))
    };
    let mut b = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(cfg.host.clone())
        .port(cfg.effective_port())
        .timeout(Some(Duration::from_secs(cfg.timeout_secs)));
    b = match cfg.security {
        Security::Starttls => b.tls(Tls::Required(tls(cfg.tls_insecure)?)),
        Security::Smtps => b.tls(Tls::Wrapper(tls(cfg.tls_insecure)?)),
        Security::None => b.tls(Tls::None),
    };
    if let (Some(u), Some(p)) = (&cfg.username, &cfg.password) {
        b = b
            .credentials(Credentials::new(u.clone(), p.clone()))
            .authentication(vec![Mechanism::Plain, Mechanism::Login]);
    }
    Ok(b.build())
}

fn classify(e: &lettre::transport::smtp::Error) -> DeliveryError {
    let msg = format!("smtp: {e}");
    if e.is_permanent() {
        DeliveryError::permanent(msg)
    } else {
        DeliveryError::transient(msg)
    }
}

/// Send the fax e-mail.
pub async fn deliver(
    cfg: &EmailConfig,
    rec: &FaxRecord,
    data_dir: &Path,
    offset: FixedOffset,
) -> Result<(), DeliveryError> {
    let msg = build_message(cfg, rec, data_dir, offset).await?;
    let tr = transport(cfg)?;
    tr.send(msg).await.map(|_| ()).map_err(|e| classify(&e))
}
