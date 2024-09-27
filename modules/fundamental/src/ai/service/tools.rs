use std::error::Error;

use crate::advisory::service::AdvisoryService;
use crate::product::service::ProductService;
use crate::vulnerability::service::VulnerabilityService;
use anyhow::anyhow;
use async_trait::async_trait;
use langchain_rust::tools::Tool;
use serde_json::Value;
use std::fmt::Write;
use trustify_common::db::query::Query;
use trustify_common::id::Id;

pub struct ToolLogger<T: Tool>(pub T);

#[async_trait]
impl<T: Tool> Tool for ToolLogger<T> {
    fn name(&self) -> String {
        self.0.name()
    }

    fn description(&self) -> String {
        self.0.description()
    }

    fn parameters(&self) -> Value {
        self.0.parameters()
    }

    async fn call(&self, input: &str) -> Result<String, Box<dyn Error>> {
        log::info!("  tool call: {}, input: {}", self.name(), input);
        let result = self.0.call(input).await;
        match &result {
            Ok(result) => {
                log::info!("     ok: {}", result);
            }
            Err(err) => {
                log::info!("     err: {}", err);
            }
        }
        result
    }

    async fn run(&self, input: Value) -> Result<String, Box<dyn Error>> {
        self.0.run(input).await
    }

    async fn parse_input(&self, input: &str) -> Value {
        self.0.parse_input(input).await
    }
}

pub struct ProductInfo(pub ProductService);

#[async_trait]
impl Tool for ProductInfo {
    fn name(&self) -> String {
        String::from("ProductInfo")
    }

    fn description(&self) -> String {
        String::from(
            r##"
This tool can be used to get information about a product.
The input should be the name of the product to search for.
When the input is a full name, the tool will provide information about the product.
When the input is a partial name, the tool will provide a list of possible matches.
"##
            .trim(),
        )
    }

    async fn run(&self, input: Value) -> Result<String, Box<dyn Error>> {
        let service = &self.0;
        let input = input
            .as_str()
            .ok_or("Input should be a string")?
            .to_string();

        let results = service
            .fetch_products(
                Query {
                    q: input,
                    ..Default::default()
                },
                Default::default(),
                (),
            )
            .await?;

        let mut result = match results.items.len() {
            0 => return Err(anyhow!("I don't know").into()),
            1 => "Found one matching product:\n",
            _ => "There are multiple products that match:\n",
        }
        .to_string();

        for product in results.items {
            writeln!(result, "  * Name: {}", product.head.name)?;
            writeln!(result, "  * UUID: {}", product.head.id)?;
            if let Some(v) = product.vendor {
                writeln!(result, "    Vendor: {}", v.head.name)?;
            }
            if !product.versions.is_empty() {
                writeln!(result, "    Versions:")?;
                for version in product.versions {
                    writeln!(result, "      * {}", version.version)?;
                }
            }
        }
        Ok(result)
    }
}

pub struct CVEInfo(pub VulnerabilityService);

#[async_trait]
impl Tool for CVEInfo {
    fn name(&self) -> String {
        String::from("CVEInfo")
    }

    fn description(&self) -> String {
        String::from(
            r##"
This tool can be used to get information about a Vulnerability.
The input should be the partial name of the Vulnerability to search for.
When the input is a full CVE ID, the tool will provide information about the vulnerability.
When the input is a partial name, the tool will provide a list of possible matches.
"##
            .trim(),
        )
    }

    async fn run(&self, input: Value) -> Result<String, Box<dyn Error>> {
        let service = &self.0;

        let input = input
            .as_str()
            .ok_or("Input should be a string")?
            .to_string();

        // is it a CVE ID?
        let mut result = "".to_string();

        let vuln = match service.fetch_vulnerability(input.as_str(), ()).await? {
            Some(v) => v,
            None => {
                // search for possible matches
                let results = service
                    .fetch_vulnerabilities(
                        Query {
                            q: input.clone(),
                            ..Default::default()
                        },
                        Default::default(),
                        (),
                    )
                    .await?;

                match results.items.len() {
                    0 => return Err(anyhow!("I don't know").into()),
                    1 => writeln!(result, "There is one advisory that matches:")?,
                    _ => writeln!(result, "There are multiple advisories that match:")?,
                }

                // let the caller know what the possible matches are
                if results.items.len() > 1 {
                    for item in results.items {
                        writeln!(result, "* Identifier: {}", item.head.identifier)?;
                        if let Some(v) = item.head.title {
                            writeln!(result, "  Title: {}", v)?;
                        }
                    }
                    return Ok(result);
                }

                // let's show the details for the one that matched.
                if let Some(v) = service
                    .fetch_vulnerability(results.items[0].head.identifier.as_str(), ())
                    .await?
                {
                    v
                } else {
                    return Err(anyhow!("I don't know").into());
                }
            }
        };

        writeln!(result, "But it had a different identifier.  Please inform the user that that you are providing information on vulnerability: {}\n", vuln.head.identifier)?;

        if vuln.head.identifier != input {
            writeln!(result, "Identifier: {}", vuln.head.identifier)?;
        }

        writeln!(result, "Identifier: {}", vuln.head.identifier)?;
        if let Some(v) = vuln.head.title {
            writeln!(result, "Title: {}", v)?;
        }
        if let Some(v) = vuln.head.description {
            writeln!(result, "Description: {}", v)?;
        }
        if let Some(v) = vuln.average_score {
            writeln!(result, "Severity: {}", v)?;
            writeln!(result, "Score: {}", v)?;
        }
        if let Some(v) = vuln.head.released {
            writeln!(result, "Released: {}", v)?;
        }

        writeln!(result, "Affected Packages:")?;
        vuln.advisories.iter().for_each(|advisory| {
            if let Some(v) = advisory.purls.get("affected") {
                v.iter().for_each(|advisory| {
                    _ = writeln!(result, "  * Name: {}", advisory.base_purl.purl);
                    _ = writeln!(result, "    Version: {}", advisory.version);
                });
            }
        });
        Ok(result)
    }
}

pub struct AdvisoryInfo(pub AdvisoryService);

#[async_trait]
impl Tool for crate::ai::service::tools::AdvisoryInfo {
    fn name(&self) -> String {
        String::from("AdvisoryInfo")
    }

    fn description(&self) -> String {
        String::from(
            r##"
This tool can be used to get information about an Advisory.
The input should be the name of the Advisory to search for.
When the input is a full name, the tool will provide information about the Advisory.
When the input is a partial name, the tool will provide a list of possible matches.
"##
            .trim(),
        )
    }

    async fn run(&self, input: Value) -> Result<String, Box<dyn Error>> {
        let service = &self.0;

        let input = input
            .as_str()
            .ok_or("Input should be a string")?
            .to_string();

        // search for possible matches
        let results = service
            .fetch_advisories(
                Query {
                    q: input,
                    ..Default::default()
                },
                Default::default(),
                (),
            )
            .await?;

        let mut result = match results.items.len() {
            0 => return Err(anyhow!("I don't know").into()),
            1 => "There is one advisory that matches:\n",
            _ => "There are multiple advisories that match:\n",
        }
        .to_string();

        // let the caller know what the possible matches are
        if results.items.len() > 1 {
            for item in results.items {
                writeln!(result, "* Identifier: {}", item.head.identifier)?;
                if let Some(v) = item.head.title {
                    writeln!(result, "  Title: {}", v)?;
                }
            }
            return Ok(result);
        }

        // let's show the details
        let item = match service
            .fetch_advisory(Id::Uuid(results.items[0].head.uuid), ())
            .await?
        {
            Some(v) => v,
            None => return Err(anyhow!("I don't know").into()),
        };

        let mut result = "".to_string();
        writeln!(result, "UUID: {}", item.head.uuid)?;
        writeln!(result, "Identifier: {}", item.head.identifier)?;
        if let Some(v) = item.head.issuer {
            writeln!(result, "Issuer: {}", v.head.name)?;
        }
        if let Some(v) = item.head.title {
            writeln!(result, "Title: {}", v)?;
        }
        if let Some(v) = item.average_score {
            writeln!(result, "Score: {}", v)?;
        }
        if let Some(v) = item.average_severity {
            writeln!(result, "Severity: {}", v)?;
        }

        writeln!(result, "Vulnerabilities:")?;
        item.vulnerabilities.iter().for_each(|v| {
            let vuln = &v.head;
            _ = writeln!(result, " * Identifier: {}", vuln.head.identifier);
            if let Some(v) = &vuln.head.title {
                _ = writeln!(result, "   Title: {}", v);
            }
            if let Some(v) = &vuln.head.description {
                _ = writeln!(result, "   Description: {}", v);
            }
            if let Some(v) = &vuln.head.released {
                _ = writeln!(result, "   Released: {}", v);
            }
        });
        Ok(result)
    }
}