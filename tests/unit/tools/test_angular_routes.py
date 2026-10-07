"""Angular Router extraction: full paths through lazy modules, navigations."""

from codegraphcontext.tools.indexing.angular_routes import (
    extract_angular_routes,
    extract_navigations,
)


def _write(tmp_path, rel, code):
    p = tmp_path / rel
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(code, encoding="utf-8")
    return {"path": str(p), "lang": "typescript", "functions": []}


def _app(tmp_path):
    files = [
        _write(tmp_path, "app/app-routing.module.ts", """
import { RouterModule, Routes } from '@angular/router';
const routes: Routes = [
  { path: '', redirectTo: 'welcome', pathMatch: 'full' },
  // { path: 'commented', component: Nope },
  { path: 'home', loadChildren: () => import('./tabs/tabs.module').then((m) => m.TabsPageModule) },
  { path: 'faqs', loadChildren: () => import('./faqs/faqs.routes').then(m => m.FAQS_ROUTES) },
  { path: `${BASE}/legacy`, component: LegacyComponent },
];
@NgModule({ imports: [RouterModule.forRoot(routes)] })
export class AppRoutingModule {}
"""),
        _write(tmp_path, "app/tabs/tabs.module.ts", """
@NgModule({ imports: [TabsPageRoutingModule] })
export class TabsPageModule {}
"""),
        _write(tmp_path, "app/tabs/tabs-routing.module.ts", """
const routes: Routes = [
  {
    path: 'tabs',
    component: TabsPage,
    children: [
      { path: 'tab1', component: Tab1Page },
      { path: 'customer/:id', component: CustomerPage },
    ],
  },
];
@NgModule({ imports: [RouterModule.forChild(routes)] })
export class TabsPageRoutingModule {}
"""),
        _write(tmp_path, "app/faqs/faqs.routes.ts", """
export const FAQS_ROUTES: Routes = [
  { path: '', loadComponent: () => import('./list.component').then(m => m.FaqsListComponent) },
  { path: 'query', component: FaqQueryComponent },
];
"""),
    ]
    return files


def test_full_paths_through_lazy_modules(tmp_path):
    routes = extract_angular_routes(_app(tmp_path), str(tmp_path))
    got = {(r["method"], r["path"], r["handler"]) for r in routes}
    assert ("REDIRECT", "/", "→ /welcome") in got
    assert ("PAGE", "/home/tabs", "TabsPage") in got
    assert ("PAGE", "/home/tabs/tab1", "Tab1Page") in got
    assert ("PAGE", "/home/tabs/customer/:id", "CustomerPage") in got
    assert ("PAGE", "/faqs", "FaqsListComponent") in got
    assert ("PAGE", "/faqs/query", "FaqQueryComponent") in got
    assert ("PAGE", "/:param/legacy", "LegacyComponent") in got
    assert not any("commented" in r["path"] for r in routes)
    assert all(r["framework"] == "angular" for r in routes)


def test_navigations_matched_to_routes(tmp_path):
    files = _app(tmp_path)
    page = _write(tmp_path, "app/tabs/tab1.page.ts", """
export class Tab1Page {
  open(id: string) {
    this.router.navigate(['/home/tabs/customer', id]);
  }
  back() { this.router.navigateByUrl('/faqs/query?x=1'); }
  rel() { this.router.navigate(['edit'], { relativeTo: this.route }); }
}
""")
    page["functions"] = [
        {"name": "open", "line_number": 3, "end_line": 5},
        {"name": "back", "line_number": 6, "end_line": 6},
        {"name": "rel", "line_number": 7, "end_line": 7},
    ]
    files.append(page)
    routes = extract_angular_routes(files, str(tmp_path))
    navs = {n["caller_name"]: n for n in extract_navigations(files, str(tmp_path), routes)}
    assert navs["open"]["target"] == "/home/tabs/customer/:param"
    assert (navs["open"]["route_path"], navs["open"]["route_handler"]) == ("/home/tabs/customer/:id", "CustomerPage")
    assert navs["back"]["route_handler"] == "FaqQueryComponent"
    assert navs["rel"]["target"] == "edit" and navs["rel"]["route_path"] == ""
