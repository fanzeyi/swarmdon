pub trait ResultExt<Ok, Err> {
    fn from_err(self) -> Result<Ok, String>;
}

impl<Ok, Err> ResultExt<Ok, Err> for Result<Ok, Err>
where
    Err: Into<anyhow::Error>,
{
    fn from_err(self) -> Result<Ok, String> {
        self.map_err(|e| e.into().to_string())
    }
}
