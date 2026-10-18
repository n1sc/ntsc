//! NTSC standard library: `sort` module.
//! Stable sort and binary search over registry arrays; all functions are
//! functional, and `sort_by`'s comparator is compiled by codegen.

use crate::registry;

fn fail(fn_name: &str, msg: impl std::fmt::Display) -> i64 {
    super::throw_str(format!("sort.{fn_name}: {msg}"))
}

/// `sort.stable_sort(arr, mode)` — `mode` selects the element type: 0 = i64,
/// 1 = f64 raw bits, 2 = strings. Returns a new array; the input is untouched.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_sort_stable_sort(handle: i64, mode: i8) -> i64 {
    let sorted = crate::ntsc_array_sort(handle, mode);
    if sorted == registry::NULL {
        fail("stable_sort", "cannot sort array");
    }
    sorted
}

/// `sort.sort_by(arr, cmp, mode)` — sorted copy using the generated
/// comparator `cmp: extern "C" fn(i64, i64) -> i8`, returning 1 when the
/// first element belongs before the second.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_sort_sort_by(
    handle: i64,
    cmp: extern "C" fn(i64, i64) -> i8,
    mode: i8,
) -> i64 {
    let cloned = registry::array_clone(handle, 0);
    if cloned == registry::NULL {
        return fail("sort_by", "cannot sort array");
    }
    // Sort the clone's own element handles. Using the source handles here
    // would overwrite the clone's freshly owned string elements and leak
    // every one of those copies when `array_write_elements` replaced its
    // vector.
    let mut elements: Vec<i64> = registry::array_to_vec(cloned);
    // The comparator may call back into the registry, so no registry borrow
    // is held across the loop: element bits are pre-read into a local vector.
    let len = elements.len();
    for i in 1..len {
        let mut j = i;
        while j > 0 {
            // Swap while the comparator does not say "a before b".
            if cmp(elements[j - 1], elements[j]) != 0 {
                break;
            }
            elements.swap(j - 1, j);
            j -= 1;
        }
    }
    let _ = mode;
    if !registry::array_write_elements(cloned, elements) {
        registry::array_drop(cloned);
        return fail("sort_by", "cannot sort array");
    }
    cloned
}

/// `sort.binary_search(arr, value, mode)` — `mode` selects the element type:
/// 0 = i64, 1 = f64 raw bits, 2 = string handle. Returns the index of
/// `value`, or -1 when absent.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_sort_binary_search(handle: i64, value: i64, mode: i8) -> i64 {
    let needle = if mode == 2 {
        registry::get_string(value)
    } else {
        None
    };
    let len = registry::array_len(handle);
    let mut low: i64 = 0;
    let mut high: i64 = len;
    while low < high {
        let mid = low + (high - low) / 2;
        let elem = registry::array_get(handle, mid).unwrap_or(0);
        let ord = match mode {
            0 => elem.cmp(&value),
            1 => {
                let a = f64::from_bits(elem as u64);
                let b = f64::from_bits(value as u64);
                a.total_cmp(&b)
            }
            2 => {
                let a = registry::get_string(elem).unwrap_or_default();
                let b = needle.as_deref().unwrap_or_default();
                a.as_str().cmp(b)
            }
            _ => {
                fail("binary_search", "unsupported element type");
                return -1;
            }
        };
        match ord {
            std::cmp::Ordering::Less => low = mid + 1,
            std::cmp::Ordering::Greater => high = mid,
            std::cmp::Ordering::Equal => return mid,
        }
    }
    -1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_int_array(values: &[i64]) -> i64 {
        let arr = registry::array_new(8, 0, 0);
        for &v in values {
            registry::array_push(arr, v);
        }
        arr
    }

    fn read_i64(arr: i64, i: i64) -> i64 {
        registry::array_get(arr, i).unwrap_or(0)
    }

    #[test]
    fn test_stable_sort_ints() {
        let arr = make_int_array(&[3, 1, 2, 3, 0]);
        let sorted = ntsc_sort_stable_sort(arr, 0);
        let values: Vec<i64> = (0..5).map(|i| read_i64(sorted, i)).collect();
        assert_eq!(values, vec![0, 1, 2, 3, 3]);

        assert_eq!(read_i64(arr, 0), 3);
        registry::array_drop(arr);
        registry::array_drop(sorted);
    }

    #[test]
    fn test_sort_by_comparator() {
        extern "C" fn ascending(a: i64, b: i64) -> i8 {
            i8::from(a <= b)
        }
        extern "C" fn descending(a: i64, b: i64) -> i8 {
            i8::from(a >= b)
        }
        let arr = make_int_array(&[3, 1, 2, 3, 0]);
        let asc = ntsc_sort_sort_by(arr, ascending, 0);
        let values: Vec<i64> = (0..5).map(|i| read_i64(asc, i)).collect();
        assert_eq!(values, vec![0, 1, 2, 3, 3]);
        let desc = ntsc_sort_sort_by(arr, descending, 0);
        let values: Vec<i64> = (0..5).map(|i| read_i64(desc, i)).collect();
        assert_eq!(values, vec![3, 3, 2, 1, 0]);

        assert_eq!(read_i64(arr, 0), 3);
        registry::array_drop(arr);
        registry::array_drop(asc);
        registry::array_drop(desc);
    }

    extern "C" fn string_ascending(a: i64, b: i64) -> i8 {
        let left = registry::get_string(a).unwrap_or_default();
        let right = registry::get_string(b).unwrap_or_default();
        i8::from(left <= right)
    }

    #[test]
    fn test_sort_by_string_elements_reclaims_clone_handles() {
        let before = registry::live_count();
        let arr = registry::array_new(8, 0, 1);
        for value in ["c", "a", "b"] {
            let source = registry::put_string(value.to_string());
            assert!(registry::array_push(arr, source));
            registry::take_string(source);
        }
        let sorted = ntsc_sort_sort_by(arr, string_ascending, 2);
        assert_eq!(
            (0..3)
                .map(|index| registry::get_string(registry::array_get(sorted, index).unwrap()))
                .collect::<Vec<_>>(),
            vec![Some("a".into()), Some("b".into()), Some("c".into())]
        );
        registry::array_drop(sorted);
        registry::array_drop(arr);
        assert_eq!(registry::live_count(), before);
    }

    #[test]
    fn test_binary_search_ints() {
        let arr = make_int_array(&[1, 3, 5, 7, 9]);
        assert_eq!(ntsc_sort_binary_search(arr, 5, 0), 2);
        assert_eq!(ntsc_sort_binary_search(arr, 6, 0), -1);
        assert_eq!(ntsc_sort_binary_search(arr, 1, 0), 0);
        assert_eq!(ntsc_sort_binary_search(arr, 9, 0), 4);
        registry::array_drop(arr);
    }

    #[test]
    fn test_binary_search_strings() {
        let arr = registry::array_new(8, 0, 1);
        for s in ["apple", "banana", "cherry"] {
            let h = registry::put_string(s.to_string());
            registry::array_push(arr, h);
            registry::take_string(h);
        }
        let needle = registry::put_string("banana".to_string());
        assert_eq!(ntsc_sort_binary_search(arr, needle, 2), 1);
        registry::take_string(needle);
        let missing = registry::put_string("grape".to_string());
        assert_eq!(ntsc_sort_binary_search(arr, missing, 2), -1);
        registry::take_string(missing);
        registry::array_drop(arr);
    }
}
