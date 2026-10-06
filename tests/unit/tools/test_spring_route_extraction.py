"""Spring (Java/Kotlin) route extraction from annotation source scans."""

from codegraphcontext.tools.indexing.route_extraction import extract_routes


def _routes(tmp_path, code, name="Controller.java", functions=()):
    f = tmp_path / name
    f.write_text(code, encoding="utf-8")
    parsed = [{"path": str(f), "lang": "java", "functions": list(functions)}]
    return {(r["method"], r["path"]) for r in extract_routes(parsed, str(tmp_path))}


def test_bare_mappings_use_class_prefix(tmp_path):
    code = '''
@RestController
@RequestMapping("/api/v1/lis/orders")
public class LisOrderController {
    @PostMapping
    public Resp create(@RequestBody Req req) { return null; }

    @GetMapping
    public List<Resp> list() { return null; }

    @GetMapping("/{id}")
    public Resp get(@PathVariable String id) { return null; }
}
'''
    assert _routes(tmp_path, code) == {
        ("POST", "/api/v1/lis/orders"),
        ("GET", "/api/v1/lis/orders"),
        ("GET", "/api/v1/lis/orders/{id}"),
    }


def test_glob_in_line_comment_does_not_swallow_rest_of_file(tmp_path):
    code = '''
@RestController
public class AccumulatePointController {
    // -- /api/v1/accumulate-points/* (internal/checkout flow) --
    @GetMapping("/api/v1/accumulate-points/customer/{customerId}")
    public Resp getByCustomer(@PathVariable String customerId) { return null; }
}
'''
    assert _routes(tmp_path, code) == {("GET", "/api/v1/accumulate-points/customer/{customerId}")}


def test_glob_in_string_literal_path(tmp_path):
    code = '''
@RestController
@RequestMapping("/uploads")
public class UploadServeController {
    @GetMapping("/**")
    public Resp serve() { return null; }

    /* @GetMapping("/commented-out") */
    // @PostMapping("/also-commented")
    @DeleteMapping("/{name}")
    public void delete(@PathVariable String name) {}
}
'''
    assert _routes(tmp_path, code) == {("GET", "/uploads/**"), ("DELETE", "/uploads/{name}")}


def test_array_named_and_multiline_arguments(tmp_path):
    code = '''
@RestController
@RequestMapping(path = "/api/einvoice")
public class EInvoiceController {
    @GetMapping({"", "/search"})
    public Resp search() { return null; }

    @PostMapping(
        value = "/issue",
        consumes = "application/json")
    public Resp issue() { return null; }

    @PutMapping(produces = "application/json")
    public Resp replace() { return null; }

    @RequestMapping(value = "/sync", method = {RequestMethod.GET, RequestMethod.POST})
    public Resp sync() { return null; }
}
'''
    assert _routes(tmp_path, code) == {
        ("GET", "/api/einvoice"),
        ("GET", "/api/einvoice/search"),
        ("POST", "/api/einvoice/issue"),
        ("PUT", "/api/einvoice"),
        ("GET", "/api/einvoice/sync"),
        ("POST", "/api/einvoice/sync"),
    }


def test_same_route_in_two_services_is_kept_for_each_file(tmp_path):
    code = '''
@RestController
public class C {
    @GetMapping("/api/v1/customers")
    public Resp list() { return null; }
}
'''
    (tmp_path / "gateway").mkdir()
    (tmp_path / "service").mkdir()
    parsed = []
    for d in ("gateway", "service"):
        f = tmp_path / d / "C.java"
        f.write_text(code, encoding="utf-8")
        parsed.append({"path": str(f), "lang": "java", "functions": []})
    routes = extract_routes(parsed, str(tmp_path))
    assert sorted(r["file"] for r in routes) == ["gateway/C.java", "service/C.java"]


def test_handler_is_nearest_function(tmp_path):
    code = '''
@RestController
public class C {
    @GetMapping("/a")
    public Resp a() { return null; }
}
'''
    f = tmp_path / "C.java"
    f.write_text(code, encoding="utf-8")
    parsed = [{"path": str(f), "lang": "java", "functions": [{"name": "a", "line_number": 4}]}]
    (route,) = extract_routes(parsed, str(tmp_path))
    assert route["handler"] == "a" and route["line"] == 4
