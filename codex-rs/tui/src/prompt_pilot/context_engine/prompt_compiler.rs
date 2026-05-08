use super::context_pack::ContextPack;

pub(crate) fn context_pack_json(context_pack: &ContextPack) -> anyhow::Result<String> {
    serde_json::to_string_pretty(context_pack).map_err(Into::into)
}
