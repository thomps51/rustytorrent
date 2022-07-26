type SharedPieceAssigner = Rc<RefCell<PieceAssigner>>;
type SharedPieceStore = Rc<RefCell<FileSystem>>; // impl Trait syntax pls
