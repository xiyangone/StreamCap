//! Run the platform's protocol module with fuel/memory limits and no WASI or host I/O.
use wasmi::{
    Caller, Config, Engine, ExternType, Func, Instance, Linker, Module, Store, StoreLimits,
    StoreLimitsBuilder, Val,
};
fn call(
    store: &mut Store<StoreLimits>,
    instance: &Instance,
    name: &str,
    args: &[i32],
) -> Result<i32, String> {
    let function = instance
        .get_func(&*store, name)
        .ok_or("咪咕签名模块缺少导出")?;
    let ty = function.ty(&*store);
    let mut output = ty
        .results()
        .iter()
        .copied()
        .map(Val::default_for_ty)
        .collect::<Vec<_>>();
    let args = args.iter().copied().map(Val::I32).collect::<Vec<_>>();
    function
        .call(store, &args, &mut output)
        .map_err(|_| "咪咕签名执行失败或超出配额")?;
    Ok(output.first().and_then(Val::i32).unwrap_or(0))
}
pub fn sign(bytes: &[u8], url: &str) -> Result<String, String> {
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("协议模块超过大小限制".into());
    }
    let mut config = Config::default();
    config.consume_fuel(true);
    let engine = Engine::new(&config);
    let module = Module::new(&engine, bytes).map_err(|_| "咪咕签名模块无效")?;
    let limits = StoreLimitsBuilder::new()
        .memory_size(32 * 1024 * 1024)
        .memories(1)
        .tables(1)
        .instances(1)
        .table_elements(4096)
        .build();
    let mut store = Store::new(&engine, limits);
    store.limiter(|s| s);
    store
        .set_fuel(20_000_000)
        .map_err(|_| "签名配额初始化失败")?;
    let mut linker = Linker::new(&engine);
    for import in module.imports() {
        if import.module() != "a" || !["a", "b", "c"].contains(&import.name()) {
            return Err("签名模块请求了不允许的系统能力".into());
        }
        let ExternType::Func(ty) = import.ty() else {
            return Err("签名模块导入无效".into());
        };
        let name = import.name().to_string();
        let types = ty.results().to_vec();
        let function = Func::new(
            &mut store,
            ty.clone(),
            move |mut caller: Caller<'_, StoreLimits>, params, results| {
                for (result, ty) in results.iter_mut().zip(types.iter()) {
                    *result = Val::default_for_ty(*ty);
                }
                if name == "a" && params.len() == 4 {
                    let index = params[1].i32().unwrap_or(-1);
                    let count = params[2].i32().unwrap_or(-1);
                    let destination = params[3].i32().unwrap_or(-1);
                    if index < 0 || destination < 0 || !(0..=1024).contains(&count) {
                        return Err(wasmi::Error::new("invalid protocol memory"));
                    }
                    let memory = caller
                        .get_export("d")
                        .and_then(wasmi::Extern::into_memory)
                        .ok_or_else(|| wasmi::Error::new("missing memory"))?;
                    let data = memory.data_mut(&mut caller);
                    let mut sum = 0_u32;
                    for i in 0..count as usize {
                        let start = index as usize + i * 8 + 4;
                        let field = data
                            .get(start..start + 4)
                            .ok_or_else(|| wasmi::Error::new("invalid vector"))?;
                        sum = sum.saturating_add(u32::from_le_bytes(
                            field
                                .try_into()
                                .map_err(|_| wasmi::Error::new("invalid vector"))?,
                        ));
                    }
                    data.get_mut(destination as usize..destination as usize + 4)
                        .ok_or_else(|| wasmi::Error::new("invalid output"))?
                        .copy_from_slice(&sum.to_le_bytes());
                }
                Ok(())
            },
        );
        linker
            .define(import.module(), import.name(), function)
            .map_err(|_| "签名导入初始化失败")?;
    }
    let instance = linker
        .instantiate_and_start(&mut store, &module)
        .map_err(|_| "签名模块启动失败")?;
    let memory = instance.get_memory(&store, "d").ok_or("签名模块缺少内存")?;
    let url = reqwest::Url::parse(url).map_err(|_| "咪咕播放地址无效")?;
    let values = url
        .query_pairs()
        .collect::<std::collections::HashMap<_, _>>();
    let mut allocations = Vec::new();
    for key in ["userid", "timestamp", "ProgramID", "Channel_ID", "puData"] {
        let value = values.get(key).map(|v| v.as_ref()).unwrap_or("");
        if value.len() > 16384 {
            return Err("签名字段超过限制".into());
        }
        let address = call(&mut store, &instance, "u", &[(value.len() + 1) as i32])?;
        memory
            .write(
                &mut store,
                address as usize,
                format!("{value}\0").as_bytes(),
            )
            .map_err(|_| "签名字段写入失败")?;
        allocations.push((address, value.len() as i32));
    }
    let protocol = b"PBTxuWiTEbUPPFcpyxs0ww==\0";
    let parameter = call(&mut store, &instance, "u", &[protocol.len() as i32])?;
    memory
        .write(&mut store, parameter as usize, protocol)
        .map_err(|_| "签名参数写入失败")?;
    let result = call(&mut store, &instance, "u", &[128])?;
    let intermediate = call(&mut store, &instance, "u", &[128])?;
    let state = call(&mut store, &instance, "m", &[])?;
    for (export, index) in [("h", 2), ("q", 1), ("p", 0)] {
        call(
            &mut store,
            &instance,
            export,
            &[state, allocations[index].0, allocations[index].1],
        )?;
    }
    call(&mut store, &instance, "j", &[state, 0, 0])?;
    call(&mut store, &instance, "r", &[state, 0, 0])?;
    for (export, index) in [("o", 4), ("i", 3)] {
        call(
            &mut store,
            &instance,
            export,
            &[state, allocations[index].0, allocations[index].1],
        )?;
    }
    call(
        &mut store,
        &instance,
        "t",
        &[
            state,
            parameter,
            protocol.len() as i32 - 1,
            intermediate,
            128,
        ],
    )?;
    let mut buffer = [0; 128];
    memory
        .read(&store, intermediate as usize, &mut buffer)
        .map_err(|_| "签名中间结果无效")?;
    let len = buffer.iter().position(|b| *b == 0).ok_or("签名长度无效")?;
    let input = call(&mut store, &instance, "u", &[(len + 1) as i32])?;
    memory
        .write(&mut store, input as usize, &buffer[..=len])
        .map_err(|_| "签名写入失败")?;
    call(&mut store, &instance, "n", &[state, input, len as i32])?;
    call(&mut store, &instance, "k", &[state, result, 128])?;
    memory
        .read(&store, result as usize, &mut buffer)
        .map_err(|_| "签名结果读取失败")?;
    let len = buffer.iter().position(|b| *b == 0).ok_or("签名长度无效")?;
    let value = std::str::from_utf8(&buffer[..len]).map_err(|_| "签名编码无效")?;
    if value.is_empty()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err("签名值无效".into());
    }
    Ok(value.to_string())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_modules_do_not_get_host_capabilities() {
        assert!(sign(b"invalid wasm", "https://example.test/").is_err());
    }
}
