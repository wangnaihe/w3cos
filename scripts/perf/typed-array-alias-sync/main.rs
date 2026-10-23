use std::{hint::black_box, time::Instant};
use w3cos_core::{Value, binary, class};
fn main() {
    eprintln!(
        "ArrayStorage_bytes={}",
        std::mem::size_of::<w3cos_core::ArrayStorage>()
    );
    let mode = std::env::args().nth(1).unwrap_or("indexed".into());
    let n: usize = std::env::args()
        .nth(2)
        .unwrap_or("2048".into())
        .parse()
        .unwrap();
    let aliases: usize = std::env::args()
        .nth(3)
        .unwrap_or("1".into())
        .parse()
        .unwrap();
    for round in 0..8 {
        let array = binary::typed_array_value(vec![Value::Number(0.0); n]);
        let buffer = array.get_property("buffer");
        let mut kept = Vec::new();
        for _ in 1..aliases {
            kept.push(class::construct(
                &binary::typed_array_class("Uint16Array"),
                vec![buffer.clone()],
            ));
        }
        let ordinary = Value::array(vec![]);
        let started = Instant::now();
        match mode.as_str() {
            "indexed" => {
                for i in 0..n {
                    binary::set_typed_array_index(&array, i, Value::Number(7.0));
                }
            }
            "fill" => {
                array.call_method("fill", vec![Value::Number(7.0)]);
            }
            "array_push" => {
                for _ in 0..n {
                    ordinary.call_method("push", vec![Value::Number(7.0)]);
                }
                black_box(&ordinary);
            }
            "construct" => {
                black_box(binary::typed_array_value(vec![Value::Number(7.0); n]));
            }
            _ => panic!("unknown mode"),
        }
        let ns = started.elapsed().as_nanos();
        let checksum: usize = binary::bytes_of(&array)
            .unwrap()
            .iter()
            .map(|v| usize::from(*v))
            .sum();
        if mode != "construct" && mode != "array_push" {
            assert_eq!(checksum, n * 7);
        }
        black_box(&kept);
        println!("{mode},{n},{aliases},{round},{ns},{checksum}");
    }
}
