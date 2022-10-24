use super::{context::Context as CpsContext, error::CpsError, free_variable, stack};
use crate::{
    analysis::{cps::continuation_type, local_variable},
    build::{self, BuildError, InstructionBuilder},
    ir::*,
    types::{CallingConvention, Type},
};
use fnv::FnvHashMap;
use std::mem::{replace, take};

const STACK_ARGUMENT_NAME: &str = "_s";
const CONTINUATION_ARGUMENT_NAME: &str = "_k";

struct Context<'a> {
    cps: &'a CpsContext,
    function_definitions: Vec<FunctionDefinition>,
}

struct Continuation {
    name: String,
    argument: Argument,
    environment: Vec<(String, Type)>,
    block: Block,
}

pub fn transform(context: &CpsContext, module: &mut Module) -> Result<(), CpsError> {
    let mut context = Context {
        cps: context,
        function_definitions: vec![],
    };

    for definition in module.function_definitions_mut() {
        transform_function_definition(&mut context, definition)?;
    }

    module
        .function_definitions_mut()
        .extend(context.function_definitions);

    Ok(())
}

fn transform_function_definition(
    context: &mut Context,
    definition: &mut FunctionDefinition,
) -> Result<(), CpsError> {
    if definition.type_().calling_convention() != CallingConvention::Source {
        return Ok(());
    }

    let continuation_type =
        continuation_type::compile(definition.result_type(), context.cps.result_type());

    definition
        .arguments_mut()
        .insert(0, Argument::new(STACK_ARGUMENT_NAME, stack::type_()));
    definition.arguments_mut().insert(
        1,
        Argument::new(CONTINUATION_ARGUMENT_NAME, continuation_type.clone()),
    );

    *definition.result_type_mut() = context.cps.result_type().clone();
    *definition.options_mut() = definition
        .options()
        .clone()
        .set_calling_convention(CallingConvention::Tail);

    // TODO Consider collecting `String` keys.
    let mut local_variables = local_variable::collect(definition)
        .into_iter()
        .map(|(name, type_)| (name.to_owned(), type_))
        .collect::<FnvHashMap<_, _>>();

    local_variables.insert(CONTINUATION_ARGUMENT_NAME.into(), continuation_type.into());

    transform_block_recursively(context, definition.body_mut(), &local_variables)?;

    Ok(())
}

fn transform_block_recursively(
    context: &mut Context,
    block: &mut Block,
    local_variables: &FnvHashMap<String, Type>,
) -> Result<(), BuildError> {
    let mut continuation_option = transform_block(context, block, local_variables)?;

    while let Some(mut continuation) = continuation_option {
        continuation_option = transform_block(context, &mut continuation.block, local_variables)?;
        create_continuation(context, continuation)?;
    }

    Ok(())
}

fn transform_block(
    context: &mut Context,
    block: &mut Block,
    local_variables: &FnvHashMap<String, Type>,
) -> Result<Option<Continuation>, BuildError> {
    let mut rest_instructions = take(block.instructions_mut());
    rest_instructions.reverse();

    while let Some(instruction) = rest_instructions.pop() {
        match instruction {
            Instruction::Call(mut call)
                if call.type_().calling_convention() == CallingConvention::Source =>
            {
                rest_instructions.reverse();

                let result_name = context.cps.name_generator().borrow_mut().generate();
                let terminal_instruction = replace(
                    block.terminal_instruction_mut(),
                    Return::new(
                        context.cps.result_type().clone(),
                        Variable::new(&result_name),
                    )
                    .into(),
                );

                if rest_instructions.is_empty()
                    && terminal_instruction
                        .to_return()
                        .map(|return_| return_.expression() == &Variable::new(call.name()).into())
                        .unwrap_or_default()
                {
                    transform_call(&mut call, CONTINUATION_ARGUMENT_NAME, result_name);
                    block.instructions_mut().push(call.into());

                    return Ok(None);
                } else {
                    let environment = get_continuation_environment(
                        &call,
                        &rest_instructions,
                        &terminal_instruction,
                        local_variables,
                    );

                    let builder = InstructionBuilder::new(context.cps.name_generator());
                    stack::push(
                        &builder,
                        build::variable(STACK_ARGUMENT_NAME, stack::type_()),
                        get_environment_record(&environment),
                    )?;
                    block.instructions_mut().extend(builder.into_instructions());

                    let name = context.cps.name_generator().borrow_mut().generate();
                    let argument = Argument::new(call.name(), call.type_().result().clone());

                    transform_call(&mut call, &name, result_name);
                    block.instructions_mut().push(call.into());

                    return Ok(Some(Continuation {
                        name,
                        argument,
                        environment,
                        block: Block::new(rest_instructions, terminal_instruction),
                    }));
                }
            }
            Instruction::If(mut if_) => {
                transform_block_recursively(context, if_.then_mut(), local_variables)?;
                transform_block_recursively(context, if_.else_mut(), local_variables)?;

                block.instructions_mut().push(if_.into());
            }
            instruction => {
                block.instructions_mut().push(instruction);
            }
        }
    }

    if let TerminalInstruction::Return(return_) = block.terminal_instruction_mut() {
        let result_name = context.cps.name_generator().borrow_mut().generate();
        let result_type = replace(return_.type_mut(), context.cps.result_type().clone());
        let result_expression =
            replace(return_.expression_mut(), Variable::new(&result_name).into());

        block.instructions_mut().push(
            Call::new(
                continuation_type::compile(&result_type, context.cps.result_type()),
                Variable::new(CONTINUATION_ARGUMENT_NAME),
                vec![Variable::new(STACK_ARGUMENT_NAME).into(), result_expression],
                result_name,
            )
            .into(),
        );
    }

    Ok(None)
}

fn transform_call(call: &mut Call, continuation_name: &str, result_name: String) {
    call.arguments_mut()
        .insert(0, Variable::new(STACK_ARGUMENT_NAME).into());
    call.arguments_mut()
        .insert(1, Variable::new(continuation_name).into());
    *call.name_mut() = result_name;
}

fn get_environment_record(environment: &[(String, Type)]) -> Record {
    build::record(
        environment
            .iter()
            .map(|(name, type_)| build::variable(name.clone(), type_.clone()))
            .collect(),
    )
}

fn create_continuation(
    context: &mut Context,
    Continuation {
        name,
        argument,
        environment,
        mut block,
    }: Continuation,
) -> Result<(), BuildError> {
    let builder = InstructionBuilder::new(context.cps.name_generator());

    let environment_record_type = get_environment_record(&environment).type_().clone();
    let environment_record = stack::pop(
        &builder,
        build::variable(STACK_ARGUMENT_NAME, stack::type_()),
        environment_record_type.clone(),
    )?;

    *block.instructions_mut() = builder
        .into_instructions()
        .into_iter()
        .chain(environment.iter().enumerate().map(|(index, (name, _))| {
            DeconstructRecord::new(
                environment_record_type.clone(),
                environment_record.expression().clone(),
                index,
                name,
            )
            .into()
        }))
        .chain(take(block.instructions_mut()))
        .collect();

    context.function_definitions.push(FunctionDefinition::new(
        &name,
        vec![Argument::new(STACK_ARGUMENT_NAME, stack::type_()), argument],
        context.cps.result_type().clone(),
        block,
        FunctionDefinitionOptions::new()
            .set_address_named(false)
            .set_calling_convention(CallingConvention::Tail)
            .set_linkage(Linkage::Internal),
    ));

    Ok(())
}

// Local variables should not include call results because they are
// passed as continuation arguments.
//
// TODO Sort fields to omit extra stack operations.
fn get_continuation_environment(
    call: &Call,
    instructions: &[Instruction],
    terminal_instruction: &TerminalInstruction,
    local_variables: &FnvHashMap<String, Type>,
) -> Vec<(String, Type)> {
    [CONTINUATION_ARGUMENT_NAME]
        .into_iter()
        .chain(free_variable::collect(instructions, terminal_instruction))
        .filter(|name| *name != call.name())
        .flat_map(|name| {
            local_variables
                .get(name)
                .map(|type_| (name.to_owned(), type_.clone()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{analysis::format, types, types::void_type};
    use pretty_assertions::assert_eq;

    fn transform_module(mut module: Module) -> Result<Module, CpsError> {
        transform(&CpsContext::new(void_type()), &mut module)?;

        Ok(module)
    }

    #[test]
    fn transform_empty() {
        assert_eq!(
            transform_module(Module::new(vec![], vec![], vec![], vec![])),
            Ok(Module::new(vec![], vec![], vec![], vec![]))
        );
    }

    #[test]
    fn transform_no_instruction() {
        assert_eq!(
            transform_module(Module::new(
                vec![],
                vec![],
                vec![],
                vec![FunctionDefinition::new(
                    "f",
                    vec![],
                    types::Primitive::Float64,
                    Block::new(vec![], TerminalInstruction::Unreachable,),
                    Default::default()
                )],
            )),
            Ok(Module::new(
                vec![],
                vec![],
                vec![],
                vec![FunctionDefinition::new(
                    "f",
                    vec![
                        Argument::new(STACK_ARGUMENT_NAME, stack::type_()),
                        Argument::new(
                            CONTINUATION_ARGUMENT_NAME,
                            continuation_type::compile(
                                &types::Primitive::Float64.into(),
                                &void_type().into()
                            )
                        )
                    ],
                    void_type(),
                    Block::new(vec![], TerminalInstruction::Unreachable,),
                    FunctionDefinitionOptions::new()
                        .set_calling_convention(types::CallingConvention::Tail)
                )],
            ))
        );
    }

    #[test]
    fn transform_if() {
        assert_eq!(
            transform_module(Module::new(
                vec![],
                vec![],
                vec![],
                vec![FunctionDefinition::new(
                    "f",
                    vec![],
                    types::Primitive::Float64,
                    Block::new(
                        vec![If::new(
                            void_type(),
                            Primitive::Boolean(true),
                            Block::new(vec![], TerminalInstruction::Unreachable),
                            Block::new(vec![], TerminalInstruction::Unreachable),
                            "_",
                        )
                        .into()],
                        TerminalInstruction::Unreachable,
                    ),
                    Default::default()
                )],
            )),
            Ok(Module::new(
                vec![],
                vec![],
                vec![],
                vec![FunctionDefinition::new(
                    "f",
                    vec![
                        Argument::new(STACK_ARGUMENT_NAME, stack::type_()),
                        Argument::new(
                            CONTINUATION_ARGUMENT_NAME,
                            continuation_type::compile(
                                &types::Primitive::Float64.into(),
                                &void_type().into()
                            )
                        )
                    ],
                    void_type(),
                    Block::new(
                        vec![If::new(
                            void_type(),
                            Primitive::Boolean(true),
                            Block::new(vec![], TerminalInstruction::Unreachable),
                            Block::new(vec![], TerminalInstruction::Unreachable),
                            "_",
                        )
                        .into()],
                        TerminalInstruction::Unreachable,
                    ),
                    FunctionDefinitionOptions::new()
                        .set_calling_convention(types::CallingConvention::Tail)
                )],
            ))
        );
    }

    #[test]
    fn transform_tail_call() {
        assert_eq!(
            transform_module(Module::new(
                vec![],
                vec![],
                vec![],
                vec![FunctionDefinition::new(
                    "f",
                    vec![],
                    types::Primitive::Float64,
                    Block::new(
                        vec![Call::new(
                            types::Function::new(
                                vec![],
                                types::Primitive::Float64,
                                types::CallingConvention::Source
                            ),
                            Variable::new("f"),
                            vec![],
                            "x",
                        )
                        .into()],
                        Return::new(types::Primitive::Float64, Variable::new("x")),
                    ),
                    Default::default()
                )],
            )),
            Ok(Module::new(
                vec![],
                vec![],
                vec![],
                vec![FunctionDefinition::new(
                    "f",
                    vec![
                        Argument::new(STACK_ARGUMENT_NAME, stack::type_()),
                        Argument::new(
                            CONTINUATION_ARGUMENT_NAME,
                            continuation_type::compile(
                                &types::Primitive::Float64.into(),
                                &void_type().into()
                            )
                        )
                    ],
                    void_type(),
                    Block::new(
                        vec![Call::new(
                            types::Function::new(
                                vec![],
                                types::Primitive::Float64,
                                types::CallingConvention::Source
                            ),
                            Variable::new("f"),
                            vec![Variable::new("_s").into(), Variable::new("_k").into()],
                            "_k_0",
                        )
                        .into()],
                        Return::new(void_type(), Variable::new("_k_0")),
                    ),
                    FunctionDefinitionOptions::new()
                        .set_calling_convention(types::CallingConvention::Tail)
                )],
            ))
        );
    }

    #[test]
    fn transform_non_tail_call() {
        insta::assert_snapshot!(format::format_module(
            &transform_module(Module::new(
                vec![],
                vec![],
                vec![],
                vec![FunctionDefinition::new(
                    "f",
                    vec![],
                    types::Primitive::Float64,
                    Block::new(
                        vec![
                            Call::new(
                                types::Function::new(
                                    vec![],
                                    types::Primitive::Float64,
                                    types::CallingConvention::Source
                                ),
                                Variable::new("f"),
                                vec![],
                                "x",
                            )
                            .into(),
                            Store::new(
                                types::Primitive::Float64,
                                Undefined::new(types::Primitive::Float64),
                                Variable::new("x")
                            )
                            .into()
                        ],
                        Return::new(types::Primitive::Float64, Variable::new("x")),
                    ),
                    Default::default()
                )],
            ))
            .unwrap()
        ));
    }

    #[test]
    fn transform_non_tail_call_in_if() {
        insta::assert_snapshot!(format::format_module(
            &transform_module(Module::new(
                vec![],
                vec![],
                vec![],
                vec![FunctionDefinition::new(
                    "f",
                    vec![],
                    types::Primitive::Float64,
                    Block::new(
                        vec![If::new(
                            void_type(),
                            Primitive::Boolean(true),
                            Block::new(
                                vec![
                                    Call::new(
                                        types::Function::new(
                                            vec![],
                                            types::Primitive::Float64,
                                            types::CallingConvention::Source
                                        ),
                                        Variable::new("f"),
                                        vec![],
                                        "x",
                                    )
                                    .into(),
                                    Store::new(
                                        types::Primitive::Float64,
                                        Undefined::new(types::Primitive::Float64),
                                        Variable::new("x")
                                    )
                                    .into()
                                ],
                                Return::new(types::Primitive::Float64, Variable::new("x"))
                            ),
                            Block::new(vec![], TerminalInstruction::Unreachable),
                            "_",
                        )
                        .into()],
                        TerminalInstruction::Unreachable,
                    ),
                    Default::default()
                )],
            ))
            .unwrap()
        ));
    }

    #[test]
    fn transform_free_variable_between_two_calls() {
        insta::assert_snapshot!(format::format_module(
            &transform_module(Module::new(
                vec![],
                vec![],
                vec![],
                vec![FunctionDefinition::new(
                    "f",
                    vec![],
                    types::Primitive::Float64,
                    Block::new(
                        vec![
                            Call::new(
                                types::Function::new(
                                    vec![],
                                    types::Primitive::Float64,
                                    types::CallingConvention::Source
                                ),
                                Variable::new("f"),
                                vec![],
                                "x",
                            )
                            .into(),
                            Load::new(
                                types::Primitive::Float64,
                                Undefined::new(types::Pointer::new(types::Primitive::Float64)),
                                "y",
                            )
                            .into(),
                            Call::new(
                                types::Function::new(
                                    vec![],
                                    types::Primitive::Float64,
                                    types::CallingConvention::Source
                                ),
                                Variable::new("f"),
                                vec![],
                                "z",
                            )
                            .into()
                        ],
                        Return::new(
                            types::Primitive::Float64,
                            ArithmeticOperation::new(
                                types::Primitive::Float64,
                                ArithmeticOperator::Add,
                                Variable::new("y"),
                                Variable::new("z")
                            )
                        ),
                    ),
                    Default::default()
                )],
            ))
            .unwrap()
        ));
    }

    #[test]
    fn transform_two_calls_with_shared_free_variable() {
        insta::assert_snapshot!(format::format_module(
            &transform_module(Module::new(
                vec![],
                vec![],
                vec![],
                vec![FunctionDefinition::new(
                    "f",
                    vec![Argument::new("x", types::Primitive::Float64)],
                    types::Primitive::Float64,
                    Block::new(
                        vec![
                            Call::new(
                                types::Function::new(
                                    vec![],
                                    types::Primitive::Float64,
                                    types::CallingConvention::Source
                                ),
                                Variable::new("f"),
                                vec![],
                                "y",
                            )
                            .into(),
                            Call::new(
                                types::Function::new(
                                    vec![],
                                    types::Primitive::Float64,
                                    types::CallingConvention::Source
                                ),
                                Variable::new("f"),
                                vec![],
                                "z",
                            )
                            .into()
                        ],
                        Return::new(types::Primitive::Float64, Variable::new("x")),
                    ),
                    Default::default()
                )],
            ))
            .unwrap()
        ));
    }

    #[test]
    fn transform_two_calls_with_two_shared_free_variables() {
        insta::assert_snapshot!(format::format_module(
            &transform_module(Module::new(
                vec![],
                vec![],
                vec![],
                vec![FunctionDefinition::new(
                    "f",
                    vec![
                        Argument::new("x", types::Primitive::Float64),
                        Argument::new("y", types::Primitive::Float64)
                    ],
                    types::Primitive::Float64,
                    Block::new(
                        vec![
                            Call::new(
                                types::Function::new(
                                    vec![],
                                    types::Primitive::Float64,
                                    types::CallingConvention::Source
                                ),
                                Variable::new("f"),
                                vec![],
                                "p",
                            )
                            .into(),
                            Call::new(
                                types::Function::new(
                                    vec![],
                                    types::Primitive::Float64,
                                    types::CallingConvention::Source
                                ),
                                Variable::new("f"),
                                vec![],
                                "q",
                            )
                            .into()
                        ],
                        Return::new(
                            types::Primitive::Float64,
                            ArithmeticOperation::new(
                                types::Primitive::Float64,
                                ArithmeticOperator::Add,
                                Variable::new("x"),
                                Variable::new("y")
                            )
                        ),
                    ),
                    Default::default()
                )],
            ))
            .unwrap()
        ));
    }

    // TODO Fix a stack element order.
    #[test]
    fn transform_two_calls_with_free_variables_shared_and_dropped() {
        insta::assert_snapshot!(format::format_module(
            &transform_module(Module::new(
                vec![],
                vec![],
                vec![],
                vec![FunctionDefinition::new(
                    "f",
                    vec![
                        Argument::new("x", types::Primitive::Float64),
                        Argument::new("y", types::Primitive::Float64)
                    ],
                    types::Primitive::Float64,
                    Block::new(
                        vec![
                            Call::new(
                                types::Function::new(
                                    vec![],
                                    types::Primitive::Float64,
                                    types::CallingConvention::Source
                                ),
                                Variable::new("f"),
                                vec![],
                                "p",
                            )
                            .into(),
                            Call::new(
                                types::Function::new(
                                    vec![types::Primitive::Float64.into()],
                                    types::Primitive::Float64,
                                    types::CallingConvention::Source
                                ),
                                Variable::new("g"),
                                vec![Variable::new("y").into()],
                                "q",
                            )
                            .into()
                        ],
                        Return::new(types::Primitive::Float64, Variable::new("x")),
                    ),
                    Default::default()
                )],
            ))
            .unwrap()
        ));
    }
}
