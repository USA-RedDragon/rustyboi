fn main() {
    let css = "
        html, body { 
            background-color: #000; 
            margin: 0px; 
            padding: 0px;
            overflow: hidden; 
            width: 100vw;
            height: 100vh;
        }
        canvas {
            display: block !important;
            position: absolute !important;
            top: 0 !important;
            left: 0 !important;
            width: 100% !important;
            height: 100% !important;
        }
    ";
    
    cargo_run_wasm::run_wasm_cli_with_css(css);
}
