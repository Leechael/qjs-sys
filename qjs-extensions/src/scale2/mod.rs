use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::{format, rc::Rc, vec::Vec};
use core::cell::{Ref, RefCell, RefMut};
use parity_scale_codec::{Compact, Decode, Encode, Output};

use js::{self as js, AsBytes, BytesOrHex, FromJsValue, ToJsValue};

use self::parser::{Enum, Id, IdInfo, PrimitiveType, String as TinyString, Type, TypeDef};

mod parser;

pub fn setup(obj: &js::Value, ctx: &js::Context) -> js::Result<()> {
    obj.define_property_fn("parseTypes", parse_types)?;
    obj.define_property_fn("appendTypes", append_types)?;
    obj.define_property_fn("encode", encode)?;
    obj.define_property_fn("encodeAll", encode_all)?;
    obj.define_property_fn("decode", decode)?;
    obj.define_property_fn("decodeAll", decode_all)?;
    obj.define_property_fn("codec", codec)?;
    ctx.eval(&js::Code::Bytecode(qjsc::compiled!(
        r#"globalThis.ScaleCodec = {
            encode(value) {
                const encoder = this.isArray ? this.scl.encodeAll : this.scl.encode;
                return encoder(value, this.ty, this.registry);
            },
            decode(value) {
                const decoder = this.isArray ? this.scl.decodeAll : this.scl.decode;
                return decoder(value, this.ty, this.registry);
            },
        };"#
    )))
    .map_err(js::Error::Custom)?;
    ctx.get_global_object()
        .get_property("ScaleCodec")?
        .set_property("scl", obj)?;
    Ok(())
}

impl js::FromJsValue for Id {
    fn from_js_value(js_value: js::Value) -> js::Result<Self> {
        if js_value.is_string() {
            let name = js::JsString::from_js_value(js_value)?;
            Ok(Id::from(name.as_str()))
        } else {
            let ind = js_value.decode_u32()?;
            Ok(Id::from(ind))
        }
    }
}

impl Enum {
    fn get_variant_by_name(&self, name: &str) -> js::Result<(&str, Option<Id>, u32)> {
        for (ind, (variant_name, tid, scale_ind)) in self.variants.iter().enumerate() {
            if variant_name == name {
                return Ok((variant_name, tid.clone(), scale_ind.unwrap_or(ind as _)));
            }
        }
        Err(js::Error::Custom(format!("Unknown variant {}", name)))
    }

    fn get_variant_by_index(&self, tag: u8) -> js::Result<(TinyString, Option<Id>)> {
        if let Some((name, ty, ind)) = self.variants.get(tag as usize) {
            match ind {
                None => return Ok((name.clone(), ty.clone())),
                Some(ind) => {
                    if tag as u32 == *ind {
                        return Ok((name.clone(), ty.clone()));
                    }
                }
            }
        };
        // fallback to linear search for custom index
        for (name, ty, ind) in self.variants.iter() {
            if let Some(ind) = ind {
                if tag as u32 == *ind {
                    return Ok((name.clone(), ty.clone()));
                }
            }
        }
        Err(js::Error::Custom(format!("Unknown variant {}", tag)))
    }
}

#[derive(Debug, Clone, Default)]
struct TypeRegistry {
    inner: Rc<RefCell<Registry>>,
}

impl TypeRegistry {
    fn borrow(&self) -> Ref<'_, Registry> {
        (*self.inner).borrow()
    }
    fn borrow_mut(&self) -> RefMut<'_, Registry> {
        (*self.inner).borrow_mut()
    }
}

impl From<Registry> for TypeRegistry {
    fn from(registry: Registry) -> Self {
        Self {
            inner: Rc::new(RefCell::new(registry)),
        }
    }
}

struct GenericLookup<'a> {
    map: BTreeMap<&'a str, &'a Id>,
}

impl<'a> GenericLookup<'a> {
    fn new(type_params: &'a [TinyString], type_args: &'a [Id]) -> Self {
        let map: BTreeMap<_, _> =
            core::iter::zip(type_params.iter().map(|t| t.as_str()), type_args.iter()).collect();
        Self { map }
    }
    fn get(&self, name: &str) -> Option<&Id> {
        self.map.get(name).copied()
    }

    fn resolve_tid(&self, tid: &Id) -> js::Result<Id> {
        match &tid.info {
            IdInfo::Name(name) => {
                if let Some(id) = self.get(name.as_str()) {
                    if !tid.type_args.is_empty() {
                        return Err(js::Error::Custom(format!(
                            "Generic type {} can not have type arguments",
                            name
                        )));
                    }
                    return Ok((*id).clone());
                }
                let mut type_args = Vec::new();
                if !tid.type_args.is_empty() {
                    for id in tid.type_args.iter() {
                        let id = self.resolve_tid(id)?;
                        type_args.push(id);
                    }
                }
                let mut tid = tid.clone();
                tid.type_args = type_args;
                Ok(tid)
            }
            IdInfo::Num(_) => Ok(tid.clone()),
            IdInfo::Type(ty) => {
                let ty = self.resolve_type(ty)?;
                Ok(Id {
                    info: IdInfo::Type(alloc::boxed::Box::new(ty)),
                    type_args: Vec::new(),
                })
            }
        }
    }

    fn resolve_type(&self, ty: &Type) -> js::Result<Type> {
        match ty {
            Type::Primitive(_) => Ok(ty.clone()),
            Type::Compact(_) => Ok(ty.clone()),
            Type::Seq(tid) => Ok(Type::Seq(self.resolve_tid(tid)?)),
            Type::Tuple(tids) => {
                let tids = tids
                    .iter()
                    .map(|tid| self.resolve_tid(tid))
                    .collect::<js::Result<Vec<_>>>()?;
                Ok(Type::Tuple(tids))
            }
            Type::Array(tid, len) => {
                let tid = self.resolve_tid(tid)?;
                Ok(Type::Array(tid, *len))
            }
            Type::Enum(def) => {
                let variants = def
                    .variants
                    .iter()
                    .map(|(name, tid, ind)| {
                        let ty = tid.as_ref().map(|tid| self.resolve_tid(tid)).transpose()?;
                        Ok((name.clone(), ty, *ind))
                    })
                    .collect::<js::Result<Vec<_>>>()?;
                Ok(Type::Enum(Enum { variants }))
            }
            Type::Struct(fields) => {
                let fields = fields
                    .iter()
                    .map(|(name, tid)| {
                        let ty = self.resolve_tid(tid)?;
                        Ok((name.clone(), ty))
                    })
                    .collect::<js::Result<Vec<_>>>()?;
                Ok(Type::Struct(fields))
            }
            Type::Alias(id) => {
                let id = self.resolve_tid(id)?;
                Ok(Type::Alias(id))
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
struct Registry {
    types: Vec<TypeDef>,
    lookup: BTreeMap<TinyString, usize>,
}

impl Registry {
    fn append(&mut self, typelist: Vec<parser::TypeDef>) -> js::Result<()> {
        for def in typelist.into_iter() {
            if let Some(name) = def.name.name.clone() {
                self.lookup.insert(name, self.types.len());
            }
            self.types.push(def);
        }
        Ok(())
    }

    fn resolve_generic(&self, tid: &Id, def: &TypeDef) -> js::Result<Type> {
        if def.name.type_params.len() != tid.type_args.len() {
            return Err(js::Error::Custom(format!(
                "Type {} expected {} type parameters, got {}",
                def.name,
                def.name.type_params.len(),
                tid.type_args.len()
            )));
        }
        if tid.type_args.is_empty() {
            return Ok(def.ty.clone());
        }
        let lookup = GenericLookup::new(&def.name.type_params, &tid.type_args);
        lookup.resolve_type(&def.ty)
    }

    fn get_type_shallow(&self, tid: &Id) -> js::Result<Type> {
        let ind = match &tid.info {
            IdInfo::Name(name) => {
                let Some(id) = self.lookup.get(name) else {
                    return match Type::primitive(name.as_str()) {
                        Some(prim) => Ok(prim.clone()),
                        None => Err(js::Error::Custom(format!("Unknown type {name}"))),
                    };
                };
                *id
            }
            IdInfo::Num(ind) => *ind as usize,
            IdInfo::Type(ty) => return Ok((**ty).clone()),
        };
        let def = self
            .types
            .get(ind)
            .ok_or(js::Error::Custom(format!("Unknown type {ind}")))?;
        self.resolve_generic(tid, def)
    }

    fn get_type(&self, tid: &Id) -> js::Result<Type> {
        let mut t = self.get_type_shallow(tid)?;
        while let Type::Alias(id) = &t {
            t = self.get_type_shallow(id)?;
        }
        Ok(t)
    }

    fn resolve_type(&self, tid: &Id, fallback: bool) -> js::Result<Type> {
        let result = self.get_type(tid);
        if result.is_ok() || !fallback {
            return result;
        }
        let IdInfo::Name(lit) = &tid.info else {
            return result;
        };
        let ty = parser::parse_type(lit)?;
        if let Type::Alias(id) = ty {
            return self.resolve_type(&id, false);
        }
        Ok(ty)
    }
}

impl js::FromJsValue for TypeRegistry {
    fn from_js_value(value: js::Value) -> js::Result<Self> {
        if value.is_undefined() {
            return Ok(Default::default());
        }
        if value.is_string() {
            let typelist = js::JsString::from_js_value(value)?;
            return parse_types_str(typelist.as_str());
        }
        let me = value
            .opaque_object_data::<Self>()
            .ok_or(js::Error::Expect("TypeRegistry"))?;
        Ok(me.clone())
    }
}

impl js::ToJsValue for TypeRegistry {
    fn to_js_value(&self, ctx: &js::Context) -> js::Result<js::Value> {
        Ok(js::Value::new_opaque_object(ctx, self.clone()))
    }
}

fn to_js_error(errs: Vec<impl core::fmt::Debug>) -> js::Error {
    let mut output = String::new();
    for err in errs {
        output.push_str(&format!("{err:?}\n"));
    }
    js::Error::Custom(output)
}

#[js::host_call]
fn parse_types(typelist: js::JsString) -> js::Result<TypeRegistry> {
    parse_types_str(typelist.as_str())
}

fn parse_types_str(typelist: &str) -> js::Result<TypeRegistry> {
    let ast = parser::parse_types(typelist)?;
    let mut registry = Registry::default();
    registry.append(ast)?;
    Ok(registry.into())
}

#[js::host_call]
fn append_types(type_registry: TypeRegistry, typelist: js::JsString) -> js::Result<()> {
    let ast = parser::parse_types(typelist.as_str())?;
    type_registry.borrow_mut().append(ast)?;
    Ok(())
}

#[js::host_call]
fn encode_all(
    value: js::Value,
    tids: Vec<Id>,
    type_registry: TypeRegistry,
) -> js::Result<AsBytes<Vec<u8>>> {
    let mut out = Vec::new();
    for (ind, tid) in tids.iter().enumerate() {
        let sub_value = value.index(ind as _)?;
        encode_value(sub_value, tid, &type_registry.borrow(), &mut out)?;
    }
    Ok(AsBytes(out))
}

#[js::host_call]
fn encode(value: js::Value, tid: Id, type_registry: TypeRegistry) -> js::Result<AsBytes<Vec<u8>>> {
    let mut out = Vec::new();
    encode_value(value, &tid, &type_registry.borrow(), &mut out)?;
    Ok(AsBytes(out))
}

fn u8a_or_hex<T>(
    value: &js::Value,
    f: impl FnOnce(&[u8]) -> js::Result<T>,
) -> Option<js::Result<T>> {
    if value.is_uint8_array() {
        let arr = match js::JsUint8Array::from_js_value(value.clone()) {
            Ok(arr) => arr,
            Err(err) => return Some(Err(err)),
        };
        let bytes = arr.as_bytes();
        return Some(f(bytes));
    }
    if value.is_string() {
        let bytes = match BytesOrHex::<Vec<u8>>::from_js_value(value.clone()) {
            Ok(bytes) => bytes.0,
            Err(err) => return Some(Err(err)),
        };
        return Some(f(&bytes));
    }
    None
}

fn encode_value(
    value: js::Value,
    tid: &Id,
    registry: &Registry,
    out: &mut impl Output,
) -> js::Result<()> {
    let t = registry.resolve_type(tid, true)?;
    match &t {
        Type::Alias(_) => unreachable!("Alias should be resolved"),
        Type::Primitive(ty) => encode_primitive(value, ty, out),
        Type::Compact(tid) => {
            let ty = registry.resolve_type(tid, false)?;
            match &ty {
                Type::Primitive(ty) => encode_compact_primitive(value, ty, out),
                Type::Tuple(tids) if tids.is_empty() => {
                    Compact(()).encode_to(out);
                    Ok(())
                }
                _ => compactable_err(),
            }
        }
        Type::Seq(tid) => {
            let ty = registry.resolve_type(tid, false)?;
            if matches!(ty, Type::Primitive(PrimitiveType::U8)) {
                let result = u8a_or_hex(&value, |bytes| {
                    bytes.encode_to(out);
                    Ok(())
                });
                if let Some(result) = result {
                    return result;
                }
            }
            let length = value.get_property("length")?.decode_u32()?;
            Compact(length).encode_to(out);
            for i in 0..length {
                encode_value(value.index(i as _)?, tid, registry, out)?;
            }
            Ok(())
        }
        Type::Tuple(ids) => {
            for (ind, ty) in ids.iter().enumerate() {
                let sub_value = value.index(ind)?;
                encode_value(sub_value, ty, registry, out)?;
            }
            Ok(())
        }
        Type::Array(ty, len) => {
            let len = *len as usize;
            let t = registry.resolve_type(ty, false)?;
            if matches!(t, Type::Primitive(PrimitiveType::U8)) {
                let result = u8a_or_hex(&value, |bytes| {
                    if bytes.len() != len {
                        return Err(js::Error::Custom(format!(
                            "Expected array of length {}, got {}",
                            len,
                            bytes.len()
                        )));
                    }
                    out.write(bytes);
                    Ok(())
                });
                if let Some(result) = result {
                    return result;
                }
            }
            let actual_len = value.length()?;
            if actual_len != len {
                return Err(js::Error::Custom(format!(
                    "Expected array of length {}, got {}",
                    len, actual_len
                )));
            }
            for ind in 0..len {
                let sub_value = value.index(ind)?;
                encode_value(sub_value, ty, registry, out)?;
            }
            Ok(())
        }
        Type::Enum(def) => {
            for entry in value.entries()? {
                let (k, v) = entry?;
                let key = js::JsString::from_js_value(k)?;
                if let Ok((_name, ty, ind)) = def.get_variant_by_name(key.as_str()) {
                    let Ok(ind) = u8::try_from(ind) else {
                        return Err(js::Error::Custom(format!(
                            "Variant index {} is too large",
                            ind
                        )));
                    };
                    ind.encode_to(out);
                    if let Some(ty) = ty {
                        encode_value(v, &ty, registry, out)?;
                    }
                    return Ok(());
                }
            }
            Err(js::Error::Custom(format!(
                "Enum with any variant of {}",
                def.variants
                    .iter()
                    .map(|(name, _, _)| name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )))
        }
        Type::Struct(fields) => {
            for (name, ty) in fields.iter() {
                let sub_value = value.get_property(name)?;
                encode_value(sub_value, ty, registry, out)?;
            }
            Ok(())
        }
    }
}

fn encode_primitive(value: js::Value, t: &PrimitiveType, out: &mut impl Output) -> js::Result<()> {
    match t {
        PrimitiveType::U8 => {
            value.decode_u8()?.encode_to(out);
        }
        PrimitiveType::U16 => {
            value.decode_u16()?.encode_to(out);
        }
        PrimitiveType::U32 => {
            value.decode_u32()?.encode_to(out);
        }
        PrimitiveType::U64 => {
            value.decode_u64()?.encode_to(out);
        }
        PrimitiveType::U128 => {
            value.decode_u128()?.encode_to(out);
        }
        PrimitiveType::I8 => {
            value.decode_i8()?.encode_to(out);
        }
        PrimitiveType::I16 => {
            value.decode_i16()?.encode_to(out);
        }
        PrimitiveType::I32 => {
            value.decode_i32()?.encode_to(out);
        }
        PrimitiveType::I64 => {
            value.decode_i64()?.encode_to(out);
        }
        PrimitiveType::I128 => {
            value.decode_i128()?.encode_to(out);
        }
        PrimitiveType::Bool => {
            value.decode_bool()?.encode_to(out);
        }
        PrimitiveType::Str => {
            js::JsString::from_js_value(value)?.as_str().encode_to(out);
        }
    }
    Ok(())
}

fn compactable_err<T>() -> js::Result<T> {
    Err(js::Error::Expect("A number or () for compact"))
}

fn encode_compact_primitive(
    value: js::Value,
    t: &PrimitiveType,
    out: &mut impl Output,
) -> js::Result<()> {
    match t {
        PrimitiveType::U8 => Compact(value.decode_u8()?).encode_to(out),
        PrimitiveType::U16 => Compact(value.decode_u16()?).encode_to(out),
        PrimitiveType::U32 => Compact(value.decode_u32()?).encode_to(out),
        PrimitiveType::U64 => Compact(value.decode_u64()?).encode_to(out),
        PrimitiveType::U128 => Compact(value.decode_u128()?).encode_to(out),
        _ => return compactable_err(),
    }
    Ok(())
}

#[js::host_call(with_context)]
fn decode(
    ctx: js::Context,
    _this: js::Value,
    value: js::JsUint8Array,
    tid: Id,
    type_registry: TypeRegistry,
) -> js::Result<js::Value> {
    decode_valude(&ctx, &mut value.as_bytes(), &tid, &type_registry.borrow())
}

#[js::host_call(with_context)]
fn decode_all(
    ctx: js::Context,
    _this: js::Value,
    value: js::JsUint8Array,
    tids: Vec<Id>,
    type_registry: TypeRegistry,
) -> js::Result<Vec<js::Value>> {
    let mut buf = value.as_bytes();
    let mut out = Vec::new();
    for tid in tids {
        let v = decode_valude(&ctx, &mut buf, &tid, &type_registry.borrow())?;
        out.push(v);
    }
    Ok(out)
}

#[js::host_call(with_context)]
fn codec(
    ctx: js::Context,
    _this: js::Value,
    tid: js::Value,
    registry: js::Value,
) -> js::Result<js::Value> {
    let obj = ctx.new_object();
    let proto = ctx.get_global_object().get_property("ScaleCodec")?;
    obj.set_prototype(&proto)?;
    obj.set_property("ty", &tid)?;
    obj.set_property("registry", &registry)?;
    obj.set_property("isArray", &js::Value::from_bool(&ctx, tid.is_array()))?;
    Ok(obj)
}

fn decode_valude(
    ctx: &js::Context,
    buf: &mut &[u8],
    ty: &Id,
    registry: &Registry,
) -> js::Result<js::Value> {
    let t = registry.resolve_type(ty, true)?;
    match &t {
        Type::Alias(_) => unreachable!("Alias should be resolved"),
        Type::Primitive(ty) => decode_primitive(ctx, buf, ty),
        Type::Compact(tid) => {
            let tid = registry.resolve_type(tid, false)?;
            match &tid {
                Type::Primitive(ty) => decode_compact_primitive(ctx, buf, ty),
                Type::Tuple(tids) if tids.is_empty() => {
                    Compact::<()>::decode(buf)
                        .map_err(|_| js::Error::Static("Unexpected end of buffer"))?;
                    Ok(ctx.new_array())
                }
                _ => compactable_err(),
            }
        }
        Type::Seq(ty) => {
            let t = registry.resolve_type(ty, false)?;
            if matches!(t, Type::Primitive(PrimitiveType::U8)) {
                let value = Vec::<u8>::decode(buf)
                    .map_err(|_| js::Error::Static("Unexpected end of buffer"))?;
                return AsBytes(value).to_js_value(ctx);
            }
            let length = Compact::<u32>::decode(buf)
                .map_err(|_| js::Error::Static("Unexpected end of buffer"))?
                .0;
            let out = ctx.new_array();
            for _ in 0..length {
                let sub_value = decode_valude(ctx, buf, ty, registry)?;
                out.array_push(&sub_value)?;
            }
            Ok(out)
        }
        Type::Tuple(types) => {
            let out = ctx.new_array();
            for ty in types {
                let sub_value = decode_valude(ctx, buf, ty, registry)?;
                out.array_push(&sub_value)?;
            }
            Ok(out)
        }
        Type::Array(ty, len) => {
            let len = *len as usize;
            let t = registry.resolve_type(ty, false)?;
            if matches!(t, Type::Primitive(PrimitiveType::U8)) {
                if buf.len() < len {
                    return Err(js::Error::Static("Unexpected end of buffer"));
                }
                let value = buf[..len].to_vec();
                *buf = &buf[len..];
                return AsBytes(value).to_js_value(ctx);
            }
            let out = ctx.new_array();
            for _ in 0..len {
                let sub_value = decode_valude(ctx, buf, ty, registry)?;
                out.array_push(&sub_value)?;
            }
            Ok(out)
        }
        Type::Enum(variants) => {
            let tag = u8::decode(buf).map_err(|_| js::Error::Static("Unexpected end of buffer"))?;
            let (variant_name, variant_type) = variants.get_variant_by_index(tag)?;
            let out = ctx.new_object();
            if let Some(variant_type) = variant_type {
                let sub_value = decode_valude(ctx, buf, &variant_type, registry)?;
                out.set_property(&variant_name, &sub_value)?;
            } else {
                out.set_property(&variant_name, &js::Value::Null)?;
            }
            Ok(out)
        }
        Type::Struct(fields) => {
            let out = ctx.new_object();
            for (name, ty) in fields {
                let sub_value = decode_valude(ctx, buf, ty, registry)?;
                out.set_property(name, &sub_value)?;
            }
            Ok(out)
        }
    }
}

fn decode_primitive(
    ctx: &js::Context,
    buf: &mut &[u8],
    t: &PrimitiveType,
) -> js::Result<js::Value> {
    macro_rules! decode_num {
        ($t: ident) => {{
            let value =
                <$t>::decode(buf).map_err(|_| js::Error::Static("Unexpected end of buffer"))?;
            value.to_js_value(ctx)
        }};
    }
    match t {
        PrimitiveType::U8 => decode_num!(u8),
        PrimitiveType::U16 => decode_num!(u16),
        PrimitiveType::U32 => decode_num!(u32),
        PrimitiveType::U64 => decode_num!(u64),
        PrimitiveType::U128 => decode_num!(u128),
        PrimitiveType::I8 => decode_num!(i8),
        PrimitiveType::I16 => decode_num!(i16),
        PrimitiveType::I32 => decode_num!(i32),
        PrimitiveType::I64 => decode_num!(i64),
        PrimitiveType::I128 => decode_num!(i128),
        PrimitiveType::Bool => decode_num!(bool),
        PrimitiveType::Str => String::decode(buf)
            .map_err(|_| js::Error::Static("Unexpected end of buffer"))?
            .to_js_value(ctx),
    }
}

fn decode_compact_primitive(
    ctx: &js::Context,
    buf: &mut &[u8],
    t: &PrimitiveType,
) -> js::Result<js::Value> {
    macro_rules! decode_num {
        ($t: ident) => {{
            let value = Compact::<$t>::decode(buf)
                .map_err(|_| js::Error::Static("Unexpected end of buffer"))?;
            value.0.to_js_value(ctx)
        }};
    }
    match t {
        PrimitiveType::U8 => decode_num!(u8),
        PrimitiveType::U16 => decode_num!(u16),
        PrimitiveType::U32 => decode_num!(u32),
        PrimitiveType::U64 => decode_num!(u64),
        PrimitiveType::U128 => decode_num!(u128),
        _ => compactable_err(),
    }
}
