use super::{AttributeSelector, Combinator, CompoundSelector, PseudoClass};

const VERSION: u8 = 1;
const MAX_ITEMS: usize = 65_535;

pub(super) fn encode(
    chain: &[CompoundSelector],
    combinators: &[Combinator],
    pseudo_element: Option<&str>,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(VERSION);
    write_optional_string(&mut out, pseudo_element);
    write_len(&mut out, chain.len());
    for compound in chain {
        let mut flags = 0;
        flags |= u8::from(compound.universal);
        flags |= u8::from(compound.any_namespace) << 1;
        flags |= u8::from(compound.unsupported) << 2;
        out.push(flags);
        write_optional_string(&mut out, compound.tag.as_deref());
        write_optional_string(&mut out, compound.id.as_deref());
        write_strings(&mut out, &compound.classes);
        write_len(&mut out, compound.attributes.len());
        for attribute in &compound.attributes {
            encode_attribute(&mut out, attribute);
        }
        write_len(&mut out, compound.pseudo_classes.len());
        for pseudo in &compound.pseudo_classes {
            encode_pseudo(&mut out, pseudo);
        }
    }
    write_len(&mut out, combinators.len());
    out.extend(combinators.iter().map(|combinator| match combinator {
        Combinator::Descendant => 0,
        Combinator::Child => 1,
        Combinator::AdjacentSibling => 2,
        Combinator::GeneralSibling => 3,
    }));
    out
}

pub(super) fn decode(
    bytes: &[u8],
) -> Option<(Vec<CompoundSelector>, Vec<Combinator>, Option<String>)> {
    let mut decoder = Decoder::new(bytes);
    if decoder.byte()? != VERSION {
        return None;
    }
    let pseudo_element = decoder.optional_string()?;
    let compound_count = decoder.count()?;
    let mut chain = Vec::with_capacity(compound_count);
    for _ in 0..compound_count {
        let flags = decoder.byte()?;
        if flags & !0b111 != 0 {
            return None;
        }
        let tag = decoder.optional_string()?;
        let id = decoder.optional_string()?;
        let classes = decoder.strings()?;
        let attribute_count = decoder.count()?;
        let mut attributes = Vec::with_capacity(attribute_count);
        for _ in 0..attribute_count {
            attributes.push(decode_attribute(&mut decoder)?);
        }
        let pseudo_count = decoder.count()?;
        let mut pseudo_classes = Vec::with_capacity(pseudo_count);
        for _ in 0..pseudo_count {
            pseudo_classes.push(decode_pseudo(&mut decoder)?);
        }
        chain.push(CompoundSelector {
            universal: flags & 1 != 0,
            any_namespace: flags & 2 != 0,
            tag,
            id,
            classes,
            attributes,
            pseudo_classes,
            unsupported: flags & 4 != 0,
        });
    }
    let combinator_count = decoder.count()?;
    let mut combinators = Vec::with_capacity(combinator_count);
    for _ in 0..combinator_count {
        combinators.push(match decoder.byte()? {
            0 => Combinator::Descendant,
            1 => Combinator::Child,
            2 => Combinator::AdjacentSibling,
            3 => Combinator::GeneralSibling,
            _ => return None,
        });
    }
    (!chain.is_empty()
        && combinators.len().checked_add(1) == Some(chain.len())
        && decoder.finished())
    .then_some((chain, combinators, pseudo_element))
}

fn encode_attribute(out: &mut Vec<u8>, attribute: &AttributeSelector) {
    match attribute {
        AttributeSelector::Present(name) => {
            out.push(0);
            write_string(out, name);
        }
        AttributeSelector::Equals(name, value, insensitive) => {
            encode_attribute_value(out, 1, name, value, *insensitive);
        }
        AttributeSelector::Includes(name, value, insensitive) => {
            encode_attribute_value(out, 2, name, value, *insensitive);
        }
        AttributeSelector::DashMatch(name, value, insensitive) => {
            encode_attribute_value(out, 3, name, value, *insensitive);
        }
        AttributeSelector::Prefix(name, value, insensitive) => {
            encode_attribute_value(out, 4, name, value, *insensitive);
        }
        AttributeSelector::Suffix(name, value, insensitive) => {
            encode_attribute_value(out, 5, name, value, *insensitive);
        }
        AttributeSelector::Substring(name, value, insensitive) => {
            encode_attribute_value(out, 6, name, value, *insensitive);
        }
    }
}

fn encode_attribute_value(out: &mut Vec<u8>, kind: u8, name: &str, value: &str, insensitive: bool) {
    out.push(kind);
    write_string(out, name);
    write_string(out, value);
    out.push(u8::from(insensitive));
}

fn decode_attribute(decoder: &mut Decoder<'_>) -> Option<AttributeSelector> {
    let kind = decoder.byte()?;
    let name = decoder.string()?;
    if kind == 0 {
        return Some(AttributeSelector::Present(name));
    }
    let value = decoder.string()?;
    let insensitive = decoder.boolean()?;
    Some(match kind {
        1 => AttributeSelector::Equals(name, value, insensitive),
        2 => AttributeSelector::Includes(name, value, insensitive),
        3 => AttributeSelector::DashMatch(name, value, insensitive),
        4 => AttributeSelector::Prefix(name, value, insensitive),
        5 => AttributeSelector::Suffix(name, value, insensitive),
        6 => AttributeSelector::Substring(name, value, insensitive),
        _ => return None,
    })
}

fn encode_pseudo(out: &mut Vec<u8>, pseudo: &PseudoClass) {
    let (kind, argument) = match pseudo {
        PseudoClass::FirstChild => (0, None),
        PseudoClass::LastChild => (1, None),
        PseudoClass::FirstOfType => (2, None),
        PseudoClass::LastOfType => (3, None),
        PseudoClass::OnlyChild => (4, None),
        PseudoClass::OnlyOfType => (5, None),
        PseudoClass::Empty => (6, None),
        PseudoClass::Root => (7, None),
        PseudoClass::Link => (8, None),
        PseudoClass::Visited => (9, None),
        PseudoClass::Target => (10, None),
        PseudoClass::Enabled => (11, None),
        PseudoClass::Disabled => (12, None),
        PseudoClass::Checked => (13, None),
        PseudoClass::Dir(value) => (14, Some(value.as_str())),
        PseudoClass::Lang(value) => (15, Some(value.as_str())),
        PseudoClass::Has(value) => (16, Some(value.as_str())),
        PseudoClass::Not(value) => (17, Some(value.as_str())),
        PseudoClass::NthChild(value) => (18, Some(value.as_str())),
        PseudoClass::NthLastChild(value) => (19, Some(value.as_str())),
        PseudoClass::NthOfType(value) => (20, Some(value.as_str())),
        PseudoClass::NthLastOfType(value) => (21, Some(value.as_str())),
    };
    out.push(kind);
    if let Some(argument) = argument {
        write_string(out, argument);
    }
}

fn decode_pseudo(decoder: &mut Decoder<'_>) -> Option<PseudoClass> {
    Some(match decoder.byte()? {
        0 => PseudoClass::FirstChild,
        1 => PseudoClass::LastChild,
        2 => PseudoClass::FirstOfType,
        3 => PseudoClass::LastOfType,
        4 => PseudoClass::OnlyChild,
        5 => PseudoClass::OnlyOfType,
        6 => PseudoClass::Empty,
        7 => PseudoClass::Root,
        8 => PseudoClass::Link,
        9 => PseudoClass::Visited,
        10 => PseudoClass::Target,
        11 => PseudoClass::Enabled,
        12 => PseudoClass::Disabled,
        13 => PseudoClass::Checked,
        14 => PseudoClass::Dir(decoder.string()?),
        15 => PseudoClass::Lang(decoder.string()?),
        16 => PseudoClass::Has(decoder.string()?),
        17 => PseudoClass::Not(decoder.string()?),
        18 => PseudoClass::NthChild(decoder.string()?),
        19 => PseudoClass::NthLastChild(decoder.string()?),
        20 => PseudoClass::NthOfType(decoder.string()?),
        21 => PseudoClass::NthLastOfType(decoder.string()?),
        _ => return None,
    })
}

fn write_strings(out: &mut Vec<u8>, values: &[String]) {
    write_len(out, values.len());
    for value in values {
        write_string(out, value);
    }
}

fn write_optional_string(out: &mut Vec<u8>, value: Option<&str>) {
    out.push(u8::from(value.is_some()));
    if let Some(value) = value {
        write_string(out, value);
    }
}

fn write_string(out: &mut Vec<u8>, value: &str) {
    write_len(out, value.len());
    out.extend_from_slice(value.as_bytes());
}

fn write_len(out: &mut Vec<u8>, value: usize) {
    let mut value = u32::try_from(value).expect("compiled selector exceeds u32 bytecode limits");
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        out.push(byte | u8::from(value != 0) << 7);
        if value == 0 {
            break;
        }
    }
}

struct Decoder<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Decoder<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn byte(&mut self) -> Option<u8> {
        let value = *self.bytes.get(self.offset)?;
        self.offset += 1;
        Some(value)
    }

    fn boolean(&mut self) -> Option<bool> {
        match self.byte()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }

    fn len(&mut self) -> Option<usize> {
        let mut value = 0u32;
        for shift in [0, 7, 14, 21, 28] {
            let byte = self.byte()?;
            if shift == 28 && byte & 0xf0 != 0 {
                return None;
            }
            value |= u32::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Some(value as usize);
            }
        }
        None
    }

    fn count(&mut self) -> Option<usize> {
        let value = self.len()?;
        (value <= MAX_ITEMS
            && value
                <= self
                    .bytes
                    .len()
                    .saturating_sub(self.offset)
                    .saturating_add(1))
        .then_some(value)
    }

    fn string(&mut self) -> Option<String> {
        let len = self.len()?;
        let end = self.offset.checked_add(len)?;
        let value = std::str::from_utf8(self.bytes.get(self.offset..end)?)
            .ok()?
            .to_string();
        self.offset = end;
        Some(value)
    }

    fn optional_string(&mut self) -> Option<Option<String>> {
        match self.byte()? {
            0 => Some(None),
            1 => Some(Some(self.string()?)),
            _ => None,
        }
    }

    fn strings(&mut self) -> Option<Vec<String>> {
        let count = self.count()?;
        let mut values = Vec::with_capacity(count);
        for _ in 0..count {
            values.push(self.string()?);
        }
        Some(values)
    }

    fn finished(&self) -> bool {
        self.offset == self.bytes.len()
    }
}
