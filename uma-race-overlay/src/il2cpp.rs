//! Минимальные биндинги к il2cpp runtime API из GameAssembly.dll.
//! Всё разрешается по именам в рантайме — статических оффсетов нет,
//! поэтому плагин переживает обновления игры, пока не меняются имена классов.

use std::ffi::{c_char, c_void, CString};
use std::sync::OnceLock;

use crate::logger::logf;

pub type RawPtr = *mut c_void;

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleA(name: *const c_char) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
}

#[allow(non_snake_case)]
pub struct Il2CppApi {
    pub domain_get: unsafe extern "C" fn() -> RawPtr,
    pub thread_attach: unsafe extern "C" fn(RawPtr) -> RawPtr,
    pub domain_assembly_open: unsafe extern "C" fn(RawPtr, *const c_char) -> RawPtr,
    pub assembly_get_image: unsafe extern "C" fn(RawPtr) -> RawPtr,
    pub class_from_name: unsafe extern "C" fn(RawPtr, *const c_char, *const c_char) -> RawPtr,
    pub class_get_method_from_name: unsafe extern "C" fn(RawPtr, *const c_char, i32) -> RawPtr,
    pub class_get_field_from_name: unsafe extern "C" fn(RawPtr, *const c_char) -> RawPtr,
    pub field_get_offset: unsafe extern "C" fn(RawPtr) -> usize,
    pub class_get_methods: unsafe extern "C" fn(RawPtr, *mut RawPtr) -> RawPtr,
    pub method_get_name: unsafe extern "C" fn(RawPtr) -> *const c_char,
    pub method_get_param_count: unsafe extern "C" fn(RawPtr) -> u32,
    pub class_get_fields: unsafe extern "C" fn(RawPtr, *mut RawPtr) -> RawPtr,
    pub field_get_name: unsafe extern "C" fn(RawPtr) -> *const c_char,
    pub object_get_class: unsafe extern "C" fn(RawPtr) -> RawPtr,
    pub class_get_name: unsafe extern "C" fn(RawPtr) -> *const c_char,
    pub class_get_element_class: unsafe extern "C" fn(RawPtr) -> RawPtr,
    pub class_is_valuetype: unsafe extern "C" fn(RawPtr) -> bool,
    pub class_array_element_size: unsafe extern "C" fn(RawPtr) -> i32,
    // Перечисление всех классов образа (для авто-поиска класса уровня гонки
    // по подстроке имени — RaceManager/Course/Program и т.п.).
    pub image_get_class_count: unsafe extern "C" fn(RawPtr) -> usize,
    pub image_get_class: unsafe extern "C" fn(RawPtr, usize) -> RawPtr,
    pub class_get_namespace: unsafe extern "C" fn(RawPtr) -> *const c_char,
    // Канонический вызов метода по MethodInfo*. Сам разруливает static/generic
    // calling convention — безопасно для синглтон-геттеров (get_Instance), где
    // прямой вызов methodPointer крашит (ждёт скрытый MethodInfo*).
    pub runtime_invoke:
        unsafe extern "C" fn(RawPtr, RawPtr, *mut RawPtr, *mut RawPtr) -> RawPtr,
}

unsafe impl Send for Il2CppApi {}
unsafe impl Sync for Il2CppApi {}

static API: OnceLock<Il2CppApi> = OnceLock::new();

pub fn api() -> Option<&'static Il2CppApi> {
    if let Some(a) = API.get() {
        return Some(a);
    }
    let module = unsafe { GetModuleHandleA(b"GameAssembly.dll\0".as_ptr() as *const c_char) };
    if module.is_null() {
        return None;
    }
    macro_rules! sym {
        ($name:literal) => {{
            let p = unsafe { GetProcAddress(module, $name.as_ptr() as *const c_char) };
            if p.is_null() {
                logf!("missing export: {}", std::str::from_utf8($name).unwrap_or("?"));
                return None;
            }
            unsafe { std::mem::transmute(p) }
        }};
    }
    let api = Il2CppApi {
        domain_get: sym!(b"il2cpp_domain_get\0"),
        thread_attach: sym!(b"il2cpp_thread_attach\0"),
        domain_assembly_open: sym!(b"il2cpp_domain_assembly_open\0"),
        assembly_get_image: sym!(b"il2cpp_assembly_get_image\0"),
        class_from_name: sym!(b"il2cpp_class_from_name\0"),
        class_get_method_from_name: sym!(b"il2cpp_class_get_method_from_name\0"),
        class_get_field_from_name: sym!(b"il2cpp_class_get_field_from_name\0"),
        field_get_offset: sym!(b"il2cpp_field_get_offset\0"),
        class_get_methods: sym!(b"il2cpp_class_get_methods\0"),
        method_get_name: sym!(b"il2cpp_method_get_name\0"),
        method_get_param_count: sym!(b"il2cpp_method_get_param_count\0"),
        class_get_fields: sym!(b"il2cpp_class_get_fields\0"),
        field_get_name: sym!(b"il2cpp_field_get_name\0"),
        object_get_class: sym!(b"il2cpp_object_get_class\0"),
        class_get_name: sym!(b"il2cpp_class_get_name\0"),
        class_get_element_class: sym!(b"il2cpp_class_get_element_class\0"),
        class_is_valuetype: sym!(b"il2cpp_class_is_valuetype\0"),
        class_array_element_size: sym!(b"il2cpp_class_array_element_size\0"),
        image_get_class_count: sym!(b"il2cpp_image_get_class_count\0"),
        image_get_class: sym!(b"il2cpp_image_get_class\0"),
        class_get_namespace: sym!(b"il2cpp_class_get_namespace\0"),
        runtime_invoke: sym!(b"il2cpp_runtime_invoke\0"),
    };
    let _ = API.set(api);
    API.get()
}

/// Загружен ли модуль (DLL) в процесс. Используется как сигнал готовности:
/// cri_ware_unity.dll грузится уже ПОСЛЕ полной инициализации il2cpp,
/// поэтому это безопасный момент, чтобы трогать рантайм (так делает Hachimi).
pub fn module_loaded(name: &[u8]) -> bool {
    !unsafe { GetModuleHandleA(name.as_ptr() as *const c_char) }.is_null()
}

/// Домен il2cpp готов (рантайм инициализирован)?
pub fn domain_ready() -> bool {
    match api() {
        Some(a) => !unsafe { (a.domain_get)() }.is_null(),
        None => false,
    }
}

/// Прикрепить текущий поток к il2cpp (обязательно перед вызовами API из своего потока).
pub fn attach_current_thread() {
    if let Some(a) = api() {
        unsafe {
            let domain = (a.domain_get)();
            if !domain.is_null() {
                (a.thread_attach)(domain);
            }
        }
    }
}

/// Найти образ сборки по имени, например "umamusume.dll".
/// Безопасный одиночный lookup (как в Hachimi), без перебора всех сборок.
pub fn find_image(name: &str) -> Option<RawPtr> {
    let a = api()?;
    let name_c = CString::new(name).ok()?;
    unsafe {
        let domain = (a.domain_get)();
        if domain.is_null() {
            return None;
        }
        let assembly = (a.domain_assembly_open)(domain, name_c.as_ptr());
        if assembly.is_null() {
            return None;
        }
        let image = (a.assembly_get_image)(assembly);
        if image.is_null() {
            None
        } else {
            Some(image)
        }
    }
}

pub fn find_class(image: RawPtr, namespace: &str, name: &str) -> Option<RawPtr> {
    let a = api()?;
    let ns = CString::new(namespace).ok()?;
    let n = CString::new(name).ok()?;
    let klass = unsafe { (a.class_from_name)(image, ns.as_ptr(), n.as_ptr()) };
    if klass.is_null() {
        logf!("class not found: {}.{}", namespace, name);
        None
    } else {
        Some(klass)
    }
}

/// Возвращает MethodInfo*. Нативный указатель кода — первое поле структуры.
pub fn find_method(klass: RawPtr, name: &str, argc: i32) -> Option<RawPtr> {
    let a = api()?;
    let n = CString::new(name).ok()?;
    let method = unsafe { (a.class_get_method_from_name)(klass, n.as_ptr(), argc) };
    if method.is_null() {
        logf!("method not found: {} ({} args)", name, argc);
        None
    } else {
        Some(method)
    }
}

/// Нативный указатель кода метода (methodPointer — первое поле MethodInfo).
pub fn method_pointer(method: RawPtr) -> RawPtr {
    unsafe { *(method as *mut RawPtr) }
}

pub fn field_offset(klass: RawPtr, name: &str) -> Option<usize> {
    let a = api()?;
    let n = CString::new(name).ok()?;
    let field = unsafe { (a.class_get_field_from_name)(klass, n.as_ptr()) };
    if field.is_null() {
        logf!("field not found: {}", name);
        None
    } else {
        Some(unsafe { (a.field_get_offset)(field) })
    }
}

/// Диагностика: выписать в лог все методы класса (когда что-то не нашлось по имени).
pub fn dump_class_methods(klass: RawPtr, label: &str) {
    let Some(a) = api() else { return };
    logf!("--- methods of {} ---", label);
    let mut iter: RawPtr = std::ptr::null_mut();
    loop {
        let method = unsafe { (a.class_get_methods)(klass, &mut iter) };
        if method.is_null() {
            break;
        }
        unsafe {
            let name = (a.method_get_name)(method);
            if !name.is_null() {
                let argc = (a.method_get_param_count)(method);
                logf!("  {} ({} args)", std::ffi::CStr::from_ptr(name).to_string_lossy(), argc);
            }
        }
    }
}

/// Прочитать System.String (Il2CppString): длина i32 по смещению 0x10, UTF-16 данные с 0x14.
pub unsafe fn read_string(s: RawPtr) -> String {
    if s.is_null() {
        return String::new();
    }
    let len = *((s as *const u8).add(0x10) as *const i32);
    if len <= 0 || len > 0x10000 {
        return String::new();
    }
    let chars = (s as *const u8).add(0x14) as *const u16;
    String::from_utf16_lossy(std::slice::from_raw_parts(chars, len as usize))
}

/// Прочитать поле экземпляра по смещению.
pub unsafe fn read_field<T: Copy>(obj: RawPtr, offset: usize) -> T {
    *((obj as *const u8).add(offset) as *const T)
}

/// Длина Il2CppArray. Layout на x64: header(0x10) + bounds(0x10) + max_length(0x18),
/// данные с 0x20. Для managed-массива длина лежит по смещению 0x18.
pub unsafe fn array_length(arr: RawPtr) -> usize {
    if arr.is_null() {
        return 0;
    }
    let len = *((arr as *const u8).add(0x18) as *const usize);
    if len > 0x10000 {
        0
    } else {
        len
    }
}

/// Элемент-ссылка Il2CppArray по индексу (для массивов ссылочных типов).
/// Данные начинаются с 0x20, каждый элемент — указатель (8 байт).
pub unsafe fn array_get_ref(arr: RawPtr, i: usize) -> RawPtr {
    if arr.is_null() {
        return std::ptr::null_mut();
    }
    let base = (arr as *const u8).add(0x20) as *const RawPtr;
    *base.add(i)
}

/// Класс объекта (il2cpp_object_get_class).
pub fn class_of(obj: RawPtr) -> RawPtr {
    match api() {
        Some(a) if !obj.is_null() => unsafe { (a.object_get_class)(obj) },
        _ => std::ptr::null_mut(),
    }
}

/// Класс ЭЛЕМЕНТА массива по самому массиву-объекту. БЕЗОПАСНО: класс берётся
/// из класса массива (через il2cpp_class_get_element_class), а не разыменованием
/// элемента — поэтому работает и для массивов value-типов (struct), не рискуя
/// прочитать мусор вместо указателя на класс.
pub fn array_element_class(arr: RawPtr) -> RawPtr {
    let Some(a) = api() else { return std::ptr::null_mut() };
    if arr.is_null() {
        return std::ptr::null_mut();
    }
    let arr_class = class_of(arr);
    if arr_class.is_null() {
        return std::ptr::null_mut();
    }
    unsafe { (a.class_get_element_class)(arr_class) }
}

/// Является ли класс value-типом (struct). Для массивов struct-элементы лежат
/// инлайн (а field offset метаданных включает 0x10 заголовка — вычитать при чтении).
pub fn is_valuetype(klass: RawPtr) -> bool {
    match api() {
        Some(a) if !klass.is_null() => unsafe { (a.class_is_valuetype)(klass) },
        _ => false,
    }
}

/// Размер элемента массива (stride) по самому массиву-объекту: для ссылочных
/// массивов = 8 (указатель), для массивов value-типов = размер struct.
pub fn array_element_size(arr: RawPtr) -> usize {
    let Some(a) = api() else { return 8 };
    let cls = class_of(arr);
    if cls.is_null() {
        return 8;
    }
    let sz = unsafe { (a.class_array_element_size)(cls) };
    if sz > 0 {
        sz as usize
    } else {
        8
    }
}

/// Указатель на данные i-го элемента массива. Для value-типов — инлайн данные
/// (arr+0x20 + i*stride); для ссылочных — разыменованный указатель элемента.
pub unsafe fn array_elem_base(arr: RawPtr, i: usize, stride: usize, is_vt: bool) -> RawPtr {
    if arr.is_null() {
        return std::ptr::null_mut();
    }
    let data = (arr as *const u8).add(0x20);
    if is_vt {
        data.add(i * stride) as RawPtr
    } else {
        *(data.add(i * 8) as *const RawPtr)
    }
}

/// Имя класса.
pub fn class_name(klass: RawPtr) -> String {
    let Some(a) = api() else { return String::new() };
    if klass.is_null() {
        return String::new();
    }
    unsafe {
        let n = (a.class_get_name)(klass);
        if n.is_null() {
            String::new()
        } else {
            std::ffi::CStr::from_ptr(n).to_string_lossy().into_owned()
        }
    }
}

/// Вызвать метод без аргументов по MethodInfo* (через il2cpp_runtime_invoke).
/// `obj` = `this` для метода экземпляра, либо null для статического. Возвращает
/// результат: для ссылочного типа — сам объект, для value-типа — boxed-объект.
/// Безопасно для generic/static геттеров, где прямой вызов methodPointer падает.
pub fn invoke0(method: RawPtr, obj: RawPtr) -> RawPtr {
    let Some(a) = api() else { return std::ptr::null_mut() };
    if method.is_null() {
        return std::ptr::null_mut();
    }
    let mut exc: RawPtr = std::ptr::null_mut();
    let ret = unsafe { (a.runtime_invoke)(method, obj, std::ptr::null_mut(), &mut exc) };
    if !exc.is_null() {
        logf!("invoke0: exception thrown ({:p})", exc);
        return std::ptr::null_mut();
    }
    ret
}

/// Вызвать метод-геттер, возвращающий int/enum (value-тип), и распаковать.
/// runtime_invoke боксирует value-результат в объект; сам int лежит по +0x10.
pub fn invoke_i32(method: RawPtr, obj: RawPtr) -> Option<i32> {
    let boxed = invoke0(method, obj);
    if boxed.is_null() {
        return None;
    }
    Some(unsafe { read_field::<i32>(boxed, 0x10) })
}

/// Namespace класса (il2cpp_class_get_namespace). Пустая строка для глобального.
/// Используется разведкой `find_classes_matching` (включается при поиске новых
/// классов уровня гонки — сейчас путь к курсу уже известен, поэтому не вызвана).
#[allow(dead_code)]
pub fn class_namespace(klass: RawPtr) -> String {
    let Some(a) = api() else { return String::new() };
    if klass.is_null() {
        return String::new();
    }
    unsafe {
        let n = (a.class_get_namespace)(klass);
        if n.is_null() {
            String::new()
        } else {
            std::ffi::CStr::from_ptr(n).to_string_lossy().into_owned()
        }
    }
}

/// Перечислить все классы образа и вернуть те, чьё имя содержит любую из
/// подстрок `needles` (без учёта регистра). Логирует каждое совпадение как
/// `ns.Name`. Чистый перебор метаданных — без разыменования объектов, безопасно.
/// Нужно для авто-поиска класса уровня гонки (RaceManager/Course/Program/…),
/// который держит course_id/track_id, но имя которого мы заранее не знаем.
#[allow(dead_code)]
pub fn find_classes_matching(image: RawPtr, needles: &[&str], label: &str) -> Vec<RawPtr> {
    let Some(a) = api() else { return Vec::new() };
    if image.is_null() {
        return Vec::new();
    }
    let lower_needles: Vec<String> = needles.iter().map(|s| s.to_lowercase()).collect();
    let count = unsafe { (a.image_get_class_count)(image) };
    logf!("--- class scan '{}' over {} classes ---", label, count);
    let mut out = Vec::new();
    for i in 0..count {
        let klass = unsafe { (a.image_get_class)(image, i) };
        if klass.is_null() {
            continue;
        }
        let name = class_name(klass);
        if name.is_empty() {
            continue;
        }
        let lname = name.to_lowercase();
        if lower_needles.iter().any(|n| lname.contains(n.as_str())) {
            let ns = class_namespace(klass);
            logf!("  match: {}.{}", ns, name);
            out.push(klass);
        }
    }
    logf!("--- class scan '{}': {} matches ---", label, out.len());
    out
}

/// Диагностика: выписать в лог все поля класса с их смещениями (для разбора
/// структуры SkillData и подобных). Помогает читать данные напрямую по offset.
pub fn dump_class_fields(klass: RawPtr, label: &str) {
    let Some(a) = api() else { return };
    logf!("--- fields of {} ({}) ---", label, class_name(klass));
    let mut iter: RawPtr = std::ptr::null_mut();
    loop {
        let field = unsafe { (a.class_get_fields)(klass, &mut iter) };
        if field.is_null() {
            break;
        }
        unsafe {
            let name = (a.field_get_name)(field);
            let offset = (a.field_get_offset)(field);
            if !name.is_null() {
                logf!(
                    "  +{:#06x} {}",
                    offset,
                    std::ffi::CStr::from_ptr(name).to_string_lossy()
                );
            }
        }
    }
}
