use super::*;

#[test]
fn decode_matches_gfx10_3_register_layout() {
    let f = decode(0x1100_0100);
    assert_eq!(f, Fields {
        zpass_increment_disable: false, perfect_zpass_counts: false, disable_conservative: false, enhanced_conservative: false,
        sample_rate: 0, zpass_enable: 1, zfail_enable: 0, sfail_enable: 0, dbfail_enable: 0, slice_even_enable: 1, slice_odd_enable: 1,
    });
    let f = decode(0xfedc_ba7f);
    assert!(f.zpass_increment_disable && f.perfect_zpass_counts && f.disable_conservative && f.enhanced_conservative);
    assert_eq!((f.sample_rate, f.zpass_enable, f.zfail_enable, f.sfail_enable, f.dbfail_enable, f.slice_even_enable, f.slice_odd_enable), (7, 0xa, 0xb, 0xc, 0xd, 0xe, 0xf));
}

#[test]
fn set_control_follows_the_release_umd() {
    let s = ListCount { log_samples: 2, ..ListCount::default() };
    assert_eq!(set_control(s, None), ListCount { control: 0x1100_0100, log_samples: 2, fixed_rate: false, custom: false });
    assert_eq!(set_control(s, Some(0x1100_1070)), ListCount { control: 0x1100_1020, log_samples: 2, fixed_rate: false, custom: true });
    assert_eq!(set_control(s, Some(0x1100_10f0)), ListCount { control: 0x1100_10f0, log_samples: 2, fixed_rate: true, custom: true });
    assert_eq!(set_control(s, Some(0x1100_1000)), ListCount { control: 0x1100_1000, log_samples: 2, fixed_rate: true, custom: true });
    let auto = set_control(s, Some(0x1100_0172));
    assert_eq!(auto.control, 0x1100_0122);
    assert_eq!(sample_state(auto, 3).control, 0x1100_0132);
    let fixed = set_control(s, Some(0x1100_0110));
    assert_eq!(sample_state(fixed, 3).control, 0x1100_0110);
    let null = set_control(s, None);
    assert_eq!(null.control & RATE_MASK, 0);
    assert_eq!(sample_state(null, 2).control, 0x1100_0120);
}

#[test]
fn set_perfect_is_ignored_after_a_custom_control() {
    let d = ListCount::default();
    assert_eq!(set_perfect(d, true).unwrap().control, 0x1100_0102);
    assert_eq!(set_perfect(d, false), None);
    assert_eq!(set_perfect(set_perfect(d, true).unwrap(), false).unwrap().control, 0x1100_0100);
    assert_eq!(set_perfect(set_control(d, Some(0x1100_1000)), true), None);
    assert_eq!(set_perfect(set_control(d, Some(0x1100_1070)), true), None);
    assert_eq!(set_perfect(set_control(set_control(d, Some(0x1100_1000)), None), true).unwrap().control, 0x1100_0102);
}

#[test]
fn reset_clears_perfect_and_sample_rate_only() {
    let s = ListCount { control: 0x1101_11f7, log_samples: 3, fixed_rate: true, custom: true };
    assert_eq!(reset(s), ListCount { control: 0x1101_1185, ..s });
}

fn ds(depth: bool, stencil: bool, written: bool) -> DsState {
    DsState { depth_test: depth, depth_written: depth, stencil_test: stencil, stencil_written: written, bounds_test: false }
}

fn dsb(depth: bool, stencil: bool, bounds: bool) -> DsState {
    DsState { depth_test: depth, depth_written: false, stencil_test: stencil, stencil_written: false, bounds_test: bounds }
}

fn w(p: Plan) -> (i64, i64, i64, i64) {
    (p.z, p.s, p.b, p.d)
}

#[test]
fn counter0_reads_only_bit0_of_each_enable_nibble() {
    let c = counter0(0x2222_2222);
    assert!(!c.zpass && !c.zfail && !c.sfail && !c.dbfail && !c.even && !c.odd);
    let p = plan(counter0(0x1100_0200), ds(true, true, false), 0, Some(0));
    assert_eq!(w(p), (0, 0, 0, 0));
    assert!(plan(counter0(0x1100_0100), ds(true, true, true), 0, Some(0)).pc_equivalent());
    assert!(plan(counter0(0xf100_0f00), ds(true, true, true), 0, Some(0)).pc_equivalent());
    assert!(zpass_only_at_rate(counter0(0x1100_0100), 0));
    assert!(!zpass_only_at_rate(counter0(0x1100_0100), 2));
    assert!(!zpass_only_at_rate(counter0(0x0100_0100), 0));
}

#[test]
fn plan_weights_decompose_categories() {
    let both = ds(true, true, false);
    let p = |c: u32, d: DsState| w(plan(counter0(c), d, 0, Some(0)));
    assert_eq!(p(0x1100_1000, both), (-1, 1, 0, 0));
    assert_eq!(p(0x1101_0000, both), (0, -1, 1, 0));
    assert_eq!(p(0x1101_1100, both), (0, 0, 1, 0));
    assert_eq!(p(0x1100_1100, both), (0, 1, 0, 0));
    assert_eq!(p(0x1110_0000, both), (0, 0, 0, 0));
    assert_eq!(p(0x1100_1000, ds(false, true, false)), (0, 0, 0, 0));
    assert_eq!(p(0x1101_0000, ds(true, false, false)), (0, 0, 0, 0));
    assert_eq!(p(0x1100_1000, ds(true, false, false)), (-1, 1, 0, 0));
    assert!(!plan(counter0(0x1100_1000), ds(true, false, false), 0, Some(0)).order_dependent);
    assert!(plan(counter0(0x1100_1000), ds(true, true, true), 0, Some(0)).order_dependent);
    assert!(!plan(counter0(0x1100_0100), ds(true, true, true), 0, Some(0)).order_dependent);
}

#[test]
fn plan_counts_dbfail_through_the_depth_bounds_pass() {
    let all = dsb(true, true, true);
    let p = |c: u32, d: DsState| w(plan(counter0(c), d, 0, Some(0)));
    assert_eq!(p(0x1110_0000, all), (0, 0, -1, 1));
    assert_eq!(p(0x1111_1100, all), (0, 0, 0, 1));
    assert_eq!(p(0x1111_0000, all), (0, -1, 0, 1));
    assert_eq!(p(0x1101_0000, all), (0, -1, 1, 0));
    assert_eq!(p(0x1110_0000, dsb(true, true, false)), (0, 0, 0, 0));
    assert_eq!(p(0x1110_0000, dsb(false, false, true)), (-1, 0, 0, 1));
    assert_eq!(p(0x1110_0100, dsb(true, false, true)), (1, -1, 0, 1));
    assert_eq!(p(0x1101_0000, dsb(true, false, true)), (0, 0, 0, 0));
    let wr = DsState { depth_written: true, ..all };
    assert!(plan(counter0(0x1110_0000), wr, 0, Some(0)).order_dependent);
    assert!(!plan(counter0(0x1110_0000), all, 0, Some(0)).order_dependent);
    let sw = DsState { stencil_written: true, ..all };
    assert!(!plan(counter0(0x1110_0000), sw, 0, Some(0)).order_dependent);
    assert!(plan(counter0(0x1101_0000), sw, 0, Some(0)).order_dependent);
}

#[test]
fn plan_gates_disable_and_slices() {
    let d = ds(true, true, false);
    assert_eq!(plan(counter0(0x1100_0101), d, 0, Some(0)), Plan::default());
    assert_eq!(plan(counter0(0x0000_0100), d, 0, Some(0)), Plan::default());
    assert!(plan(counter0(0x0100_0100), d, 0, Some(0)).pc_equivalent());
    assert_eq!(plan(counter0(0x0100_0100), d, 0, Some(1)), Plan::default());
    assert_eq!(plan(counter0(0x1000_0100), d, 0, Some(2)), Plan::default());
    assert!(plan(counter0(0x1000_0100), d, 0, Some(3)).pc_equivalent());
    let u = plan(counter0(0x0100_0100), d, 0, None);
    assert!(u.slice_unknown && u.pc_equivalent());
    assert!(!plan(counter0(0x1100_0100), d, 0, None).slice_unknown);
    assert!(slice_gate(counter0(0x1000_0100), Some(5)) && !slice_gate(counter0(0x1000_0100), Some(4)));
}

#[test]
fn plan_scales_by_sample_rate() {
    let d = ds(true, true, false);
    let p = plan(counter0(0x1100_0120), d, 0, Some(0));
    assert_eq!((p.z, p.shl, p.shr), (1, 2, 0));
    assert!(!p.pc_equivalent());
    let p = plan(counter0(0x1100_0100), d, 2, Some(0));
    assert_eq!((p.shl, p.shr), (0, 2));
    assert!(plan(counter0(0x1100_0120), d, 2, Some(0)).pc_equivalent());
}

#[test]
fn failed_replay_falls_back_to_plain_zpass() {
    let p = plan(counter0(0x1100_1000), ds(true, true, false), 2, Some(0));
    assert_eq!((p.z, p.s), (-1, 1));
    let f = zpass_fallback(p);
    assert_eq!((f.z, f.s, f.b, f.d, f.shl, f.shr), (1, 0, 0, 0, 0, 2));
    assert!(!f.needs_replay());
    for c in [0x1100_1000u32, 0x1101_0000, 0x1101_1000, 0x1100_1100, 0x1101_1100, 0x1110_0000] {
        let f = zpass_fallback(plan(counter0(c), dsb(true, true, true), 0, Some(0)));
        assert!(f.pc_equivalent());
    }
}

#[test]
fn closed_lists_reject_like_the_debug_umd() {
    assert_eq!(recordable(false), None);
    assert_eq!(recordable(true), Some("Cannot record commands into a command list that has already been closed."));
}

#[test]
fn combine_cpu_matches_the_shader_contract() {
    assert_eq!(combine_cpu(0, &[(vec![(1024, -1), (2048, 1)], 0, 0)], false), 1024);
    assert_eq!(combine_cpu(5, &[(vec![(4096, 1)], 2, 0)], false), 5 + 16384);
    assert_eq!(combine_cpu(0, &[(vec![(4097, 1)], 0, 2)], false), 1024);
    assert_eq!(combine_cpu(0, &[(vec![(10, -1), (10, 1)], 0, 0)], true), 0);
    assert_eq!(combine_cpu(0, &[(vec![(10, -1), (11, 1)], 0, 0)], true), 1);
    assert_eq!(combine_cpu(1, &[], true), 1);
    assert_eq!(combine_cpu(0, &[(vec![(7, -1), (9, 1), (3, -1), (5, 1)], 0, 0)], false), 4);
    let k = combine_constants(0, 8, 1, 2, true, &[16, 24, 32, 40], &[-1, 3, -1, 1]);
    assert_eq!(k, [0, 8, 4, 1, 2, 1, 16, 24, 32, 40, 0xffff_ffff, 3, 0xffff_ffff, 1]);
    let k = combine_constants(1, 8, 0, 0, false, &[0], &[]);
    assert_eq!(&k[..7], &[1, 8, 1, 0, 0, 0, 0]);
}

#[test]
fn batches_split_groups_without_breaking_one() {
    let g = |n: usize| Group { aux: (0..n as u32).map(|s| (s, 1)).collect(), shl: 0, shr: 0 };
    let gs = vec![g(3), g(3), g(2), g(4)];
    assert_eq!(batches(&gs, 6), vec![0..2, 2..4]);
    assert_eq!(batches(&gs, 5), vec![0..1, 1..3, 3..4]);
    assert_eq!(batches(&gs, 254), vec![0..4]);
    assert_eq!(batches(&[g(4)], 2), vec![0..1]);
    assert!(batches(&[], 4).is_empty());
    let many: Vec<Group> = (0..200).map(|_| g(3)).collect();
    let b = batches(&many, BLOCK_ENTRIES as usize);
    assert!(b.iter().all(|r| many[r.clone()].iter().map(|x| x.aux.len()).sum::<usize>() <= BLOCK_ENTRIES as usize));
    assert_eq!(b.iter().map(|r| r.len()).sum::<usize>(), 200);
}

#[test]
fn slot_pool_reuses_only_after_the_fence() {
    let never = |_: usize, _: u64| false;
    let mut p = SlotPool::default();
    let a = p.take(&never);
    let b = p.take(&never);
    assert_eq!((a, b), (0, 1));
    p.hold(7, a);
    p.hold(7, b);
    p.addref(a);
    p.note_exec(&[7], 50, 3);
    p.note_exec(&[7], 50, 5);
    p.release(7);
    assert!(p.is_live(a) && !p.is_pending(a));
    assert!(p.is_pending(b));
    assert_eq!(p.take(&never), 2);
    p.unref(a);
    assert!(p.is_pending(a));
    let at4 = |f: usize, v: u64| f == 50 && v <= 4;
    assert_eq!(p.take(&at4), 3);
    let at5 = |f: usize, v: u64| f == 50 && v <= 5;
    let r = p.take(&at5);
    assert!(r == a || r == b);
    let r2 = p.take(&never);
    assert!((r2 == a || r2 == b) && r2 != r);
    p.hold(9, 10);
    p.release(9);
    assert!(!p.is_live(10) && !p.is_pending(10));
    assert_eq!(p.take(&never), 10);
    p.untake(4);
    assert_eq!(p.take(&never), 4);
    assert!(!p.holds_any(&[7, 9]));
}

fn put(d: &mut [u8], o: usize, v: u32) {
    d[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

fn desc_with(depth: u32, func: u32, stencil: u32, wmask: u8, ops: [u32; 3], samples: u32) -> Vec<u8> {
    let mut d = vec![0u8; D_LEN];
    put(&mut d, D_DS + DS_DEPTH_ENABLE, depth);
    put(&mut d, D_DS + DS_DEPTH_WRITE, 1);
    put(&mut d, D_DS + DS_DEPTH_FUNC, func);
    put(&mut d, D_DS + DS_STENCIL_ENABLE, stencil);
    d[D_DS + DS_STENCIL_WRITE_MASK] = wmask;
    for (i, o) in ops.iter().enumerate() {
        put(&mut d, D_DS + DS_FRONT + 4 * i, *o);
        put(&mut d, D_DS + DS_BACK + 4 * i, 1);
    }
    for i in 0..8 {
        d[D_BLEND + BLEND_RT_MASK + i * BLEND_RT_STRIDE] = 0xf;
    }
    put(&mut d, D_SAMPLE_COUNT, samples);
    d
}

#[test]
fn ds_state_and_variants_from_the_pc_desc() {
    let d = desc_with(1, 2, 1, 0xff, [1, 1, 3], 4);
    let full = DsState { depth_test: true, depth_written: true, stencil_test: true, stencil_written: true, bounds_test: false };
    assert_eq!(ds_from_desc(&d), Some(full));
    assert_eq!(log2_samples(&d), Some(2));
    assert_eq!(ds_from_desc(&desc_with(1, FUNC_ALWAYS, 1, 0, [1, 1, 3], 1)), Some(DsState { depth_test: false, stencil_written: false, ..full }));
    assert_eq!(ds_from_desc(&desc_with(0, 2, 0, 0xff, [3, 3, 3], 1)), Some(DsState::default()));
    assert_eq!(ds_from_desc(&desc_with(1, 2, 1, 0xff, [1, 1, 1], 1)), Some(DsState { stencil_written: false, ..full }));
    let mut always = desc_with(1, 2, 1, 0xff, [3, 3, 3], 1);
    put(&mut always, D_DS + DS_FRONT + 12, FUNC_ALWAYS);
    assert_eq!(ds_from_desc(&always), Some(full));
    put(&mut always, D_DS + DS_BACK + 12, FUNC_ALWAYS);
    assert_eq!(ds_from_desc(&always), Some(DsState { stencil_test: false, stencil_written: false, ..full }));
    let mut v = d.clone();
    assert!(variant_desc(&mut v, Pass::StencilOnly));
    assert_eq!((u32_at(&v, D_DS + DS_DEPTH_WRITE), u32_at(&v, D_DS + DS_DEPTH_FUNC), u32_at(&v, D_DS + DS_STENCIL_ENABLE), v[D_DS + DS_STENCIL_WRITE_MASK]), (0, FUNC_ALWAYS, 1, 0));
    assert!((0..8).all(|i| v[D_BLEND + BLEND_RT_MASK + i * BLEND_RT_STRIDE] == 0));
    let mut v = d.clone();
    assert!(variant_desc(&mut v, Pass::Coverage));
    assert_eq!(u32_at(&v, D_DS + DS_STENCIL_ENABLE), 0);
    let mut so = d.clone();
    put(&mut so, D_SO_ENTRIES, 2);
    so[D_SO_ENTRIES - 8..D_SO_ENTRIES].copy_from_slice(&0x1234u64.to_le_bytes());
    let mut a = so.clone();
    assert!(variant_desc(&mut a, Pass::Raster));
    assert_eq!(&a[D_SO_ENTRIES - 8..D_SO_ENTRIES + 4], &so[D_SO_ENTRIES - 8..D_SO_ENTRIES + 4]);
    assert!(info_from_desc(&so).unwrap().has_so);
    assert!(!variant_desc(&mut vec![0u8; 16], Pass::Coverage));
}

fn bc(b: &[u8]) -> Vec<u8> {
    let mut v = (b.as_ptr() as u64).to_le_bytes().to_vec();
    v.extend_from_slice(&(b.len() as u64).to_le_bytes());
    v
}

fn stream_of(parts: &[(u32, Vec<u8>)]) -> (Vec<u8>, Vec<(u32, usize)>) {
    let mut s = Vec::new();
    let mut subs = Vec::new();
    for (t, b) in parts {
        append_sub(&mut s, &mut subs, *t, b);
    }
    assert_eq!(crate::mesh_pso::parse(&s).unwrap(), subs);
    (s, subs)
}

#[test]
fn stream_info_reads_depth_bounds_samples_and_view_instancing() {
    let vs = vec![1u8; 8];
    let ps = vec![2u8; 8];
    let mut ds1 = default_ds_block().to_vec();
    put(&mut ds1, DS_BOUNDS, 1);
    put(&mut ds1, DS_DEPTH_WRITE, 0);
    let (s, subs) = stream_of(&[(SUB_VS, bc(&vs)), (SUB_PS, bc(&ps)), (SUB_DEPTH_STENCIL1, ds1.clone()), (SUB_SAMPLE_DESC, vec![4, 0, 0, 0, 0, 0, 0, 0])]);
    let i = info_from_stream(&s, &subs);
    assert_eq!(i.ds, DsState { depth_test: true, depth_written: false, stencil_test: false, stencil_written: false, bounds_test: true });
    assert_eq!(i.log, 2);
    assert_eq!(i.stages, vec![(SUB_VS, vs.as_ptr() as u64, 8), (SUB_PS, ps.as_ptr() as u64, 8)]);
    assert!(!i.has_so && !i.view_instancing);
    let (s, subs) = stream_of(&[(SUB_VS, bc(&vs))]);
    let i = info_from_stream(&s, &subs);
    assert_eq!(i.ds, DsState { depth_test: true, depth_written: true, ..DsState::default() });
    assert_eq!(i.log, 0);
    let mut vi = vec![0u8; 24];
    put(&mut vi, 0, 2);
    let (s, subs) = stream_of(&[(SUB_VS, bc(&vs)), (SUB_VIEW_INSTANCING, vi)]);
    assert!(info_from_stream(&s, &subs).view_instancing);
}

#[test]
fn pso_sample_count_cache_follows_a_reused_pointer() {
    let vs = vec![1u8; 8];
    let (s4, _) = stream_of(&[(SUB_VS, bc(&vs)), (SUB_SAMPLE_DESC, vec![4, 0, 0, 0, 0, 0, 0, 0])]);
    let (s8, _) = stream_of(&[(SUB_VS, bc(&vs)), (SUB_SAMPLE_DESC, vec![8, 0, 0, 0, 0, 0, 0, 0])]);
    let key = 0x5eed_0000usize;
    assert_eq!(pso_log(key), None);
    note_stream_pso(key, &s4, &[]);
    assert_eq!(pso_log(key), Some(2));
    assert_eq!(pso_log(key), Some(2));
    note_stream_pso(key, &s8, &[]);
    assert_eq!(pso_log(key), Some(3));
    assert_eq!(pso_log(0), None);
}

#[test]
fn stream_variants_edit_or_append_state() {
    let vs = vec![1u8; 8];
    let ps = vec![2u8; 8];
    let alt = vec![3u8; 12];
    let mut ds1 = default_ds_block().to_vec();
    put(&mut ds1, DS_BOUNDS, 1);
    put(&mut ds1, DS_STENCIL_ENABLE, 1);
    let (s, subs) = stream_of(&[(SUB_VS, bc(&vs)), (SUB_PS, bc(&ps)), (SUB_DEPTH_STENCIL1, ds1), (SUB_BLEND, default_blend()), (SUB_CACHED_PSO, bc(&[9u8; 4]))]);
    let o = find_sub(&subs, SUB_DEPTH_STENCIL1).unwrap();
    for (pass, stencil, bounds) in [(Pass::StencilOnly, 1, 1), (Pass::Coverage, 0, 1), (Pass::Raster, 0, 0)] {
        let v = variant_stream(&s, &subs, pass, None);
        assert_eq!(v.len(), s.len());
        assert_eq!((u32_at(&v, o + DS_DEPTH_WRITE), u32_at(&v, o + DS_DEPTH_FUNC), v[o + DS_STENCIL_WRITE_MASK]), (0, FUNC_ALWAYS, 0));
        assert_eq!((u32_at(&v, o + DS_STENCIL_ENABLE), u32_at(&v, o + DS_BOUNDS)), (stencil, bounds), "{pass:?}");
        let b = find_sub(&subs, SUB_BLEND).unwrap();
        assert!((0..8).all(|i| v[b + BLEND_RT_MASK + i * BLEND_RT_STRIDE] == 0));
        let c = find_sub(&subs, SUB_CACHED_PSO).unwrap();
        assert!(v[c..c + 16].iter().all(|&x| x == 0));
    }
    let v = variant_stream(&s, &subs, Pass::Raster, Some(&alt));
    let p = find_sub(&subs, SUB_PS).unwrap();
    assert_eq!((u64_at(&v, p), u64_at(&v, p + 8)), (alt.as_ptr() as u64, 12));
    let (s, subs) = stream_of(&[(SUB_VS, bc(&vs))]);
    let v = variant_stream(&s, &subs, Pass::Coverage, None);
    let vs2 = crate::mesh_pso::parse(&v).unwrap();
    let d = find_sub(&vs2, SUB_DEPTH_STENCIL).unwrap();
    let b = find_sub(&vs2, SUB_BLEND).unwrap();
    assert_eq!((u32_at(&v, d + DS_DEPTH_ENABLE), u32_at(&v, d + DS_DEPTH_FUNC), u32_at(&v, d + DS_STENCIL_ENABLE)), (1, FUNC_ALWAYS, 0));
    assert!((0..8).all(|i| v[b + BLEND_RT_MASK + i * BLEND_RT_STRIDE] == 0));
    assert_eq!(u32_at(&v, b + 8 + 8), 2);
}

#[test]
fn stream_snapshot_owns_its_pointed_to_arrays() {
    let name = b"POSITION\0".to_vec();
    let mut elems = vec![0u8; 64];
    elems[0..8].copy_from_slice(&(name.as_ptr() as u64).to_le_bytes());
    elems[32..40].copy_from_slice(&(name.as_ptr() as u64).to_le_bytes());
    let mut il = vec![0u8; 16];
    il[0..8].copy_from_slice(&(elems.as_ptr() as u64).to_le_bytes());
    put(&mut il, 8, 2);
    let mut decl = vec![0u8; 24];
    decl[8..16].copy_from_slice(&(name.as_ptr() as u64).to_le_bytes());
    let strides = 16u32.to_le_bytes().to_vec();
    let mut so = vec![0u8; 32];
    so[0..8].copy_from_slice(&(decl.as_ptr() as u64).to_le_bytes());
    put(&mut so, 8, 1);
    so[16..24].copy_from_slice(&(strides.as_ptr() as u64).to_le_bytes());
    put(&mut so, 24, 1);
    let (s, _) = stream_of(&[(SUB_INPUT_LAYOUT, il), (SUB_SO, so)]);
    let r = snapshot_stream(&s, &[]).unwrap();
    drop((elems, decl, strides, name));
    let o = find_sub(&r.subs, SUB_INPUT_LAYOUT).unwrap();
    let e = u64_at(&r.stream, o) as *const u8;
    let copy = unsafe { core::slice::from_raw_parts(e, 64) };
    for k in [0usize, 32] {
        let n = u64_at(copy, k) as *const core::ffi::c_char;
        assert_eq!(unsafe { core::ffi::CStr::from_ptr(n) }.to_bytes(), b"POSITION");
    }
    let so = find_sub(&r.subs, SUB_SO).unwrap();
    let d = unsafe { core::slice::from_raw_parts(u64_at(&r.stream, so) as *const u8, 24) };
    assert_eq!(unsafe { core::ffi::CStr::from_ptr(u64_at(d, 8) as *const core::ffi::c_char) }.to_bytes(), b"POSITION");
    assert_eq!(unsafe { *(u64_at(&r.stream, so + 16) as *const u32) }, 16);
    assert!(info_from_stream(&r.stream, &r.subs).has_so);
    assert!(snapshot_stream(&[0xff, 0, 0, 0], &[]).is_none());
}

#[test]
fn psv0_reports_pixel_shader_uavs() {
    let plain = tombox_xshader::dxc::compile("float4 main() : SV_Target { return 1; }", "ps_6_0").expect("dxc");
    assert_eq!(ps_has_uav(&plain), Some(false));
    assert_eq!(shader_kind(&plain), Some(0));
    let tex = tombox_xshader::dxc::compile("Texture2D t : register(t0); float4 main(float4 p : SV_Position) : SV_Target { return t.Load(int3(p.xy, 0)); }", "ps_6_0").expect("dxc");
    assert_eq!(ps_has_uav(&tex), Some(false));
    let uav = tombox_xshader::dxc::compile("RWByteAddressBuffer u : register(u1); float4 main(float4 p : SV_Position) : SV_Target { u.Store(0, 1); return 1; }", "ps_6_0").expect("dxc");
    assert_eq!(ps_has_uav(&uav), Some(true));
    assert_eq!(ps_has_uav(b"not a container at all, really not"), None);
}

#[test]
fn rt_array_index_writers_are_detected() {
    let vs = tombox_xshader::dxc::compile("float4 main(uint v : SV_VertexID) : SV_Position { return float4(v, 0, 0, 1); }", "vs_6_0").expect("dxc");
    assert_eq!(shader_kind(&vs), Some(1));
    assert_eq!(writes_rt_array_index(&vs), Some(false));
    let gs = tombox_xshader::dxc::compile(
        "struct O { float4 p : SV_Position; uint s : SV_RenderTargetArrayIndex; }; [maxvertexcount(3)] void main(triangle float4 i[3] : SV_Position, inout TriangleStream<O> t) { for (uint k = 0; k < 3; k++) { O o; o.p = i[k]; o.s = 1; t.Append(o); } }",
        "gs_6_0",
    )
    .expect("dxc");
    assert_eq!(writes_rt_array_index(&gs), Some(true));
    let ps = tombox_xshader::dxc::compile("float4 main(uint s : SV_RenderTargetArrayIndex) : SV_Target { return s; }", "ps_6_0").expect("dxc");
    assert_eq!(writes_rt_array_index(&ps), Some(false));
    let info = Info { stages: vec![(SUB_GS, gs.as_ptr() as u64, gs.len() as u64), (SUB_PS, ps.as_ptr() as u64, ps.len() as u64)], ..Info::default() };
    assert!(rtai_written(&info));
    let info = Info { stages: vec![(SUB_VS, vs.as_ptr() as u64, vs.len() as u64), (SUB_PS, ps.as_ptr() as u64, ps.len() as u64)], ..Info::default() };
    assert!(!rtai_written(&info));
}

#[test]
fn dsv_first_slice_follows_the_view_dimension() {
    let mk = |dim: u32, a: u32, b: u32| {
        let mut d = vec![0u8; 24];
        put(&mut d, 4, dim);
        put(&mut d, 12, a);
        put(&mut d, 16, b);
        d
    };
    assert_eq!(dsv_first_slice(None), 0);
    assert_eq!(dsv_first_slice(Some(&mk(3, 9, 9))), 0);
    assert_eq!(dsv_first_slice(Some(&mk(2, 1, 5))), 5);
    assert_eq!(dsv_first_slice(Some(&mk(4, 1, 7))), 7);
    assert_eq!(dsv_first_slice(Some(&mk(6, 3, 8))), 3);
    assert_eq!(dsv_first_slice(Some(&mk(5, 3, 8))), 0);
    note_dsv(0xd5_0000, Some(&mk(4, 0, 3)));
    note_dsv(0xd5_0040, Some(&mk(4, 0, 4)));
    copy_dsvs(2, 0xe5_0000, 0xd5_0000, 0x40);
    assert_eq!((dsv_slice(0xe5_0000), dsv_slice(0xe5_0040)), (Some(3), Some(4)));
    copy_dsvs(1, 0xe5_0000, 0xf5_0000, 0x40);
    assert_eq!(dsv_slice(0xe5_0000), None);
}

#[test]
fn install_device_chains_dsv_creation_and_copies() {
    let mut v: Vec<usize> = (0..48).map(|i| 9000 + i).collect();
    let (pd, pc) = (PREV_DSV.load(Ordering::Relaxed), PREV_COPY_SIMPLE.load(Ordering::Relaxed));
    assert!(install_device(&mut v));
    assert_eq!((v[DEV_CREATE_DSV], v[DEV_COPY_SIMPLE]), (w_create_dsv as usize, w_copy_simple as usize));
    assert_eq!(PREV_DSV.load(Ordering::Relaxed), 9021);
    assert!(!install_device(&mut v));
    assert!(!install_device(&mut vec![0usize; 8]));
    PREV_DSV.store(pd, Ordering::Relaxed);
    PREV_COPY_SIMPLE.store(pc, Ordering::Relaxed);
}

const UAV_PS: &str = r#"RWByteAddressBuffer u : register(u1);
RWTexture2D<float4> t : register(u2);
RWStructuredBuffer<uint> sb : register(u3);
RWByteAddressBuffer tb_gfault : register(u4);
cbuffer C : register(b0) { uint k; };
float4 main(float4 p : SV_Position) : SV_Target {
    uint o;
    tb_gfault.InterlockedAdd(0, 1u, o);
    if (o == 0u) tb_gfault.Store2(4, uint2(1, 2));
    u.Store(0, asuint(p.x));
    u.InterlockedAdd(8, 1u);
    InterlockedMax(sb[k], 3u);
    t[uint2(p.xy)] = float4(1, 0, 0, 1);
    sb[k + 1] += 2;
    float4 r = t[uint2(0, 0)] + u.Load(4) + sb[0];
    if (k > 2) { sb[2] = 5; } else u.Store2(16, uint2(0, 0));
    return r + o;
}
"#;

#[test]
fn strip_uav_writes_removes_every_store_and_keeps_reads() {
    let s = strip::strip_uav_writes(UAV_PS).expect("strip");
    for gone in ["u.Store(", "u.InterlockedAdd", "InterlockedMax", "t[uint2(p.xy)] =", "sb[k + 1] +=", "sb[2] = 5", "u.Store2", "tb_gfault.Store2", "tb_gfault.InterlockedAdd"] {
        assert!(!s.contains(gone), "{gone} survived:\n{s}");
    }
    for kept in ["RWByteAddressBuffer u__tbro", "RWTexture2D<float4> t__tbro", "t__tbro[uint2(0, 0)]", "u__tbro.Load(4)", "sb__tbro[0]", "o = tb_gfault__tbro.Load(0);", "if (o == 0u) {}"] {
        assert!(s.contains(kept), "{kept} missing:\n{s}");
    }
    let code = tombox_xshader::dxc::compile(&s, "ps_6_0").expect("stripped shader compiles");
    assert!(ps_has_uav(&code).is_some());
    assert_eq!(strip::uav_names(UAV_PS).iter().map(|n| n.1.as_str()).collect::<Vec<_>>(), vec!["u", "t", "sb", "tb_gfault"]);
}

#[test]
fn strip_uav_writes_refuses_what_it_cannot_reproduce() {
    let head = "RWByteAddressBuffer u : register(u1);\nRWStructuredBuffer<uint> sb : register(u2);\nAppendStructuredBuffer<uint> ap : register(u3);\n";
    let bad = [
        "float4 main() : SV_Target { uint o; u.InterlockedAdd(0, 1u, o); return o; }",
        "float4 main() : SV_Target { uint o; InterlockedAdd(sb[0], 1u, o); return o; }",
        "float4 main() : SV_Target { sb.IncrementCounter(); return 0; }",
        "float4 main() : SV_Target { sb[0]++; return 0; }",
        "float4 main() : SV_Target { uint x = (sb[0] = 1); return x; }",
        "void f(inout uint a) { a = 1; } float4 main() : SV_Target { f(sb[0]); return 0; }",
        "float4 main() : SV_Target { RWByteAddressBuffer c = u; return 0; }",
    ];
    for b in bad {
        assert!(strip::strip_uav_writes(&format!("{head}{b}")).is_err(), "{b}");
    }
    assert!(strip::strip_uav_writes("float4 main() : SV_Target { return 0; }").is_err());
    let ok = strip::strip_uav_writes(&format!("{head}// u.Store(0, 1);\nfloat4 main() : SV_Target {{ return sb[0]; }}")).unwrap();
    assert!(ok.contains("// u.Store(0, 1);") && ok.contains("sb__tbro[0]"));
}

#[test]
fn translated_pixel_shaders_with_uavs_keep_their_source() {
    let uav = tombox_xshader::dxc::compile("RWByteAddressBuffer u : register(u1); float4 main(float4 p : SV_Position) : SV_Target { u.Store(0, 1); return 1; }", "ps_6_0").expect("dxc");
    let plain = tombox_xshader::dxc::compile("float4 main() : SV_Target { return 1; }", "ps_6_0").expect("dxc");
    note_translated(&plain, b"blob", 1, &[0; 8]);
    note_translated(&uav, b"blob", 2, &[0; 8]);
    let m = PS_SRC.lock().unwrap();
    let m = m.as_ref().unwrap();
    assert!(!m.get(&(plain.as_ptr() as usize)).is_some_and(|e| (e.0, e.1) == (plain.len(), fnv1a(&plain))));
    let e = m.get(&(uav.as_ptr() as usize)).unwrap();
    assert_eq!((e.0, e.1), (uav.len(), fnv1a(&uav)));
}

#[test]
fn strip_ps_compiles_a_store_free_variant_once() {
    let src = "RWByteAddressBuffer u : register(u1); float4 main(float4 p : SV_Position) : SV_Target { u.Store(0, 1); return u.Load(4); }";
    let code = tombox_xshader::dxc::compile(src, "ps_6_0").expect("dxc");
    assert!(strip_ps(&code).is_err());
    PS_STRIP.lock().unwrap().as_mut().unwrap().clear();
    note_ps_hlsl(&code, src, "ps_6_0");
    let a = strip_ps(&code).expect("variant");
    let b = strip_ps(&code).unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    assert_eq!(&a[0..4], b"DXBC");
    let info = Info { stages: vec![(SUB_PS, code.as_ptr() as u64, code.len() as u64)], ..Info::default() };
    assert!(ps_override(&info).unwrap().is_some_and(|v| Arc::ptr_eq(&v, &a)));
    assert!(fallback_reason(Some(&info)).is_none());
    let vsu = tombox_xshader::dxc::compile("RWByteAddressBuffer u : register(u1); float4 main() : SV_Position { u.Store(0, 1); return 0; }", "vs_6_0").expect("dxc");
    let info = Info { stages: vec![(SUB_VS, vsu.as_ptr() as u64, vsu.len() as u64)], ..Info::default() };
    assert!(fallback_reason(Some(&info)).unwrap().contains("stage 1"));
}

#[test]
fn combine_shader_compiles_with_its_root_signature() {
    let code = tombox_xshader::dxc::compile(COMBINE_HLSL, "cs_6_0").expect("dxc");
    assert_eq!(&code[0..4], b"DXBC");
    assert!(code.windows(4).any(|w| w == b"RTS0"));
}

#[test]
fn install_chains_and_owns_slots_64_65() {
    let _g = crate::cl2_state::TEST_PREV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let vt: Vec<AtomicUsize> = (0..128).map(|i| AtomicUsize::new(7000 + i)).collect();
    let saved: Vec<usize> = PREV.iter().map(|p| p.load(Ordering::Relaxed)).collect();
    let n = install(&vt);
    assert_eq!(n, plan_slots().len() + 2);
    assert_eq!(vt[X_SET_DB_COUNT_CONTROL].load(Ordering::Relaxed), w_set_control as usize);
    assert_eq!(vt[X_SET_PERFECT_ZPASS].load(Ordering::Relaxed), w_set_perfect as usize);
    assert_eq!(vt[X_DRAW].load(Ordering::Relaxed), w_draw as usize);
    assert_eq!(vt[X_DRAW_INDEXED_X].load(Ordering::Relaxed), w_draw_indexed_x as usize);
    assert_eq!(vt[X_EXEC_INDIRECT].load(Ordering::Relaxed), w_exec_indirect as usize);
    assert_eq!(vt[X_EXEC_BUNDLE].load(Ordering::Relaxed), w_exec_bundle as usize);
    assert_eq!(vt[X_DISPATCH_MESH].load(Ordering::Relaxed), 7127);
    assert_eq!(PREV[X_DRAW].load(Ordering::Relaxed), 7012);
    assert_eq!(PREV[X_RESOLVE_QUERY].load(Ordering::Relaxed), 7030);
    let fam = crate::console_slots_mesh::list_plan(crate::console_slots_dev2::Family::Scarlett);
    let mvt: Vec<AtomicUsize> = (0..128).map(|_| AtomicUsize::new(1)).collect();
    for &(s, f) in &fam {
        mvt[s].store(f, Ordering::Relaxed);
    }
    let ms = mesh_slots(&mvt);
    assert!(ms.iter().all(|&(s, _)| s == X_DISPATCH_MESH_X || s == X_DISPATCH_MESH));
    assert_eq!(ms.len(), fam.iter().filter(|&&(s, _)| s == X_DISPATCH_MESH_X || s == X_DISPATCH_MESH).count());
    for (i, v) in saved.into_iter().enumerate() {
        PREV[i].store(v, Ordering::Relaxed);
    }
}

#[test]
fn slot64_and_65_entry_points_track_list_state() {
    let fake = Box::new([0usize; 4]);
    let l = &*fake as *const [usize; 4] as *mut c_void;
    unsafe {
        w_set_perfect(l, 1);
        assert_eq!(list_count(l).control, 0x1100_0102);
        let v = 0x1100_1070u32;
        w_set_control(l, &v);
        assert_eq!(list_count(l), ListCount { control: 0x1100_1000, log_samples: 0, fixed_rate: false, custom: true });
        w_set_perfect(l, 1);
        assert_eq!(list_count(l).control, 0x1100_1000);
        w_set_control(l, core::ptr::null());
        assert_eq!(list_count(l), ListCount::default());
    }
}

#[test]
fn active_counter_tracks_open_occlusion_queries() {
    let before = ACTIVE.load(Ordering::Relaxed);
    let mut l = ListDb::default();
    set_active(&mut l, Some((1, 0, 0)));
    set_active(&mut l, Some((1, 0, 1)));
    assert_eq!(ACTIVE.load(Ordering::Relaxed), before + 1);
    set_active(&mut l, None);
    set_active(&mut l, None);
    assert_eq!(ACTIVE.load(Ordering::Relaxed), before);
}

#[path = "dbcount_hw.rs"]
mod hw;
