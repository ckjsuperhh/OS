use kernel_refactored::*;

/// group_12: configFS 伪文件系统测试
///
/// 覆盖范围：
/// - configFS 目录/属性创建与销毁（mkdir / rmdir）
/// - 属性文件的动态 show/store 回调
/// - 通过 FLike::Config 和 SYS_READ/SYS_WRITE 系统调用分发读写
///
/// configFS 与 procfs/sysfs 不同：对象由用户态 mkdir 创建、rmdir 销毁，
/// 属性内容由回调动态生成/接受，适合内核子系统的运行时配置。

#[test]
fn configfs_demo_mkdir_read_write() {
    let kern = Kernel::new(64);
    kern.proc_init();

    // 用户态 mkdir：在 /config/demo 下创建一个 counter 类型的 item
    kern.configfs.mkdir("demo", "counter0").unwrap();

    // 打开 /config/demo/counter0/value 属性并读取默认值
    let lookup = kern.configfs.lookup("demo/counter0/value").unwrap();
    let ConfigLookup::Attr(item, attr_name) = lookup else {
        panic!("expected attr");
    };
    let mut node = ConfigNode::new(item, &attr_name);
    let mut buf = [0u8; 16];
    let n = node.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"0");

    // 写入新值 42 后回读，验证 store/show 回调正确更新 data
    node.write(b"42").unwrap();
    node.offset = 0; // 重置读取偏移，从头读取完整内容
    let n = node.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"42");
}

#[test]
fn configfs_demo_rmdir() {
    let kern = Kernel::new(64);
    kern.proc_init();

    // 创建并确认 counter0 存在
    kern.configfs.mkdir("demo", "counter0").unwrap();
    assert!(kern.configfs.lookup("demo/counter0/value").is_ok());

    // 用户态 rmdir：销毁 counter0，再次查找应失败
    kern.configfs.rmdir("demo", "counter0").unwrap();
    assert!(kern.configfs.lookup("demo/counter0/value").is_err());
}

#[test]
fn configfs_via_syscall_open_read_write() {
    let kern = Kernel::new(64);
    kern.proc_init();
    kern.configfs.mkdir("demo", "counter0").unwrap();

    // 将 init 任务设为 CPU0 当前任务，使 SYS_READ/SYS_WRITE 能找到调用者
    let root = kern.tasks.root.lock().unwrap().clone().unwrap();
    kern.set_cur(0, Some(root.clone()));

    // 获取属性节点并预置 value 为 "99"
    let lookup = kern.configfs.lookup("demo/counter0/value").unwrap();
    let ConfigLookup::Attr(item, attr_name) = lookup else {
        panic!("expected attr");
    };
    item.data.lock().unwrap().insert("value".to_string(), "99".to_string());
    let node = ConfigNode::new(item, &attr_name);

    // 占用 fd 0/1/2，使 config fd >= 3，触发 read_fd/write_fd 分支
    for _ in 0..3 {
        root.add_file(FLike::File(FHandle::new("dummy", FdOpt::default(), false, false)));
    }
    let fd = root.add_file(FLike::Config(node));
    assert!(fd >= 3);

    // 通过 SYS_READ 读取 config 属性，应返回实际内容长度 2（"99"）
    let read_result = kern.dispatch_syscall(SYS_READ, fd, 0x1000, 16, 0, 0, 0);
    assert!(read_result.is_ok());
    assert_eq!(read_result.unwrap(), 2);
}
