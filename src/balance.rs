use serde::Deserialize;

#[derive(Clone, Copy, Debug)]
pub enum Provider {
    DeepSeek,
    ProxyApi,
    OpenRouter,
}

impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Provider::DeepSeek => "DEEPSEEK",
            Provider::ProxyApi => "PROXYAPI",
            Provider::OpenRouter => "OPENROUTER",
        }
    }

    fn url(self) -> &'static str {
        match self {
            Provider::DeepSeek => "https://api.deepseek.com/user/balance",
            Provider::ProxyApi => "https://api.proxyapi.ru/proxyapi/balance",
            Provider::OpenRouter => "https://openrouter.ai/api/v1/credits",
        }
    }
}

#[derive(Deserialize)]
struct DeepSeekBalanceInfo {
    total_balance: String,
    currency: String,
}

#[derive(Deserialize)]
struct DeepSeekResponse {
    balance_infos: Vec<DeepSeekBalanceInfo>,
}

#[derive(Deserialize)]
struct ProxyApiResponse {
    balance: f64,
}

#[derive(Deserialize)]
struct OpenRouterCredits {
    total_credits: f64,
    total_usage: f64,
}

#[derive(Deserialize)]
struct OpenRouterResponse {
    data: OpenRouterCredits,
}

pub fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .use_rustls_tls()
        .build()
        .map_err(|e| format!("Client build: {}", e))
}

/// Whole display line for one provider. Errors render as text: the overlay is
/// the only diagnostic channel this app has.
pub async fn fetch_line(client: &reqwest::Client, provider: Provider, token: &str) -> String {
    match fetch(client, provider, token).await {
        Ok((amount, currency)) => {
            format!("{}: {}", provider.label(), format_money(amount, &currency))
        }
        Err(e) => format!("{}: {}", provider.label(), e),
    }
}

async fn fetch(
    client: &reqwest::Client,
    provider: Provider,
    token: &str,
) -> Result<(f64, String), String> {
    let resp = client
        .get(provider.url())
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .map_err(|e| format!("Request: {}", e))?;

    let status = resp.status();
    if status == 401 || status == 403 {
        return Err("Invalid token".into());
    }

    let body = resp.text().await.map_err(|e| format!("Read: {}", e))?;

    // ProxyAPI answers 200 with this in the body instead of a 401.
    if body.contains("Invalid API Key") {
        return Err("Invalid token".into());
    }

    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }

    match provider {
        Provider::DeepSeek => {
            let data: DeepSeekResponse = parse(&body)?;
            let info = data
                .balance_infos
                .first()
                .ok_or_else(|| "Empty response".to_string())?;
            let bal = info
                .total_balance
                .parse::<f64>()
                .map_err(|_| "Balance parse".to_string())?;
            Ok((bal, info.currency.clone()))
        }
        Provider::ProxyApi => {
            let data: ProxyApiResponse = parse(&body)?;
            Ok((data.balance, "RUB".into()))
        }
        Provider::OpenRouter => {
            let data: OpenRouterResponse = parse(&body)?;
            Ok((data.data.total_credits - data.data.total_usage, "USD".into()))
        }
    }
}

fn parse<T: serde::de::DeserializeOwned>(body: &str) -> Result<T, String> {
    serde_json::from_str(body).map_err(|e| format!("Parse: {}", e))
}

pub fn format_money(amount: f64, currency: &str) -> String {
    let sym = currency_symbol(currency);
    match currency {
        // ponytail: only the ruble trails here; add more if a user complains
        "RUB" => format!("{} {}", format_balance(amount), sym),
        _ => format!("{} {}", sym, format_balance(amount)),
    }
}

pub fn currency_symbol(currency: &str) -> &str {
    match currency {
        "USD" => "$",
        "EUR" => "\u{20AC}",
        "RUB" => "\u{20BD}",
        "CNY" => "\u{00A5}",
        "GBP" => "\u{00A3}",
        "JPY" => "\u{00A5}",
        "KRW" => "\u{20A9}",
        _ => currency,
    }
}

pub fn format_balance(value: f64) -> String {
    // Round once, in cents: splitting trunc/fract first turns 12.999 into "12.100".
    let cents = (value.abs() * 100.0).round() as i64;
    let (int_part, frac) = (cents / 100, cents % 100);

    let mut formatted_int = String::new();
    for (i, ch) in int_part.to_string().chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            formatted_int.push(' ');
        }
        formatted_int.push(ch);
    }
    let int_display: String = formatted_int.chars().rev().collect();

    let sign = if value < 0.0 && cents > 0 { "-" } else { "" };
    format!("{}{}.{:02}", sign, int_display, frac)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balance_formatting() {
        assert_eq!(format_balance(12.999), "13.00"); // used to print "12.100"
        assert_eq!(format_balance(12.34), "12.34");
        assert_eq!(format_balance(1234567.5), "1 234 567.50");
        assert_eq!(format_balance(-42.5), "-42.50");
        assert_eq!(format_balance(-0.001), "0.00");
        assert_eq!(format_money(5.0, "USD"), "$ 5.00");
        assert_eq!(format_money(5.0, "RUB"), "5.00 \u{20BD}");
    }
}
