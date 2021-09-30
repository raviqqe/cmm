use super::error::CpsTransformationError;
use super::stack::STACK_TYPE;
use crate::{
    analysis::cps::stack,
    build::{self, InstructionBuilder, NameGenerator, TypedExpression},
    ir::*,
    types::{self, CallingConvention, Type},
};
use std::{cell::RefCell, rc::Rc};

struct Context {
    pub name_generator: Rc<RefCell<NameGenerator>>,
    pub function_definitions: Vec<FunctionDefinition>,
    pub result_type: Type,
}

pub fn compile(module: &Module, result_type: &Type) -> Result<Module, CpsTransformationError> {
    let mut context = Context {
        name_generator: Rc::new(NameGenerator::new("_cps_").into()),
        function_definitions: vec![],
        result_type: result_type.clone(),
    };

    Ok(Module::new(
        module.variable_declarations().to_vec(),
        module.function_declarations().to_vec(),
        module.variable_definitions().to_vec(),
        module
            .function_definitions()
            .iter()
            .map(|definition| compile_definition(&mut context, definition))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .chain(context.function_definitions)
            .collect(),
    ))
}

fn compile_definition(
    context: &mut Context,
    definition: &FunctionDefinition,
) -> Result<FunctionDefinition, CpsTransformationError> {
    Ok(
        if definition.type_().calling_convention() == CallingConvention::Target {
            FunctionDefinition::new(
                definition.name(),
                definition.arguments().to_vec(),
                compile_block(context, definition.body())?,
                definition.result_type().clone(),
                definition.calling_convention(),
                definition.linkage(),
            )
        } else {
            definition.clone()
        },
    )
}

fn compile_block(context: &mut Context, block: &Block) -> Result<Block, CpsTransformationError> {
    Ok(Block::new(
        block
            .instructions()
            .iter()
            .map(|instruction| compile_instruction(context, instruction))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect(),
        block.terminal_instruction().clone(),
    ))
}

fn compile_instruction(
    context: &mut Context,
    instruction: &Instruction,
) -> Result<Vec<Instruction>, CpsTransformationError> {
    Ok(match instruction {
        Instruction::Call(call) => {
            if call.type_().calling_convention() == CallingConvention::Source {
                compile_source_function_call(context, call)?
            } else {
                vec![call.clone().into()]
            }
        }
        Instruction::If(if_) => vec![If::new(
            if_.type_().clone(),
            if_.condition().clone(),
            compile_block(context, if_.then())?,
            compile_block(context, if_.else_())?,
            if_.name(),
        )
        .into()],
        _ => vec![instruction.clone()],
    })
}

fn compile_source_function_call(
    context: &mut Context,
    call: &Call,
) -> Result<Vec<Instruction>, CpsTransformationError> {
    let builder = InstructionBuilder::new(context.name_generator.clone());

    let stack_pointer = stack::create_stack(&builder)?;
    let result_pointer = builder.allocate_stack(call.type_().result().clone());

    stack::push_to_stack(&builder, stack_pointer.clone(), result_pointer.clone())?;

    builder.call(
        TypedExpression::new(call.function().clone(), call.type_().clone()),
        vec![
            stack_pointer.clone(),
            compile_continuation(context, call.type_().result())?,
        ]
        .into_iter()
        .chain(
            call.arguments()
                .iter()
                .zip(call.type_().arguments())
                .map(|(expression, type_)| TypedExpression::new(expression.clone(), type_.clone())),
        )
        .collect(),
    )?;
    let result = builder.load(result_pointer)?;

    stack::destroy_stack(&builder, stack_pointer)?;

    Ok(builder
        .into_instructions()
        .into_iter()
        .chain(vec![PassThrough::new(
            result.type_().clone(),
            result.expression().clone(),
            call.name(),
        )
        .into()])
        .collect())
}

fn compile_continuation(
    context: &mut Context,
    result_type: &Type,
) -> Result<TypedExpression, CpsTransformationError> {
    let name = context.name_generator.borrow_mut().generate();

    context.function_definitions.push(FunctionDefinition::new(
        &name,
        vec![
            Argument::new("stack", STACK_TYPE.clone()),
            Argument::new("result", result_type.clone()),
        ],
        {
            let builder = InstructionBuilder::new(context.name_generator.clone());

            let result_pointer = stack::pop_from_stack(
                &builder,
                build::variable("stack", STACK_TYPE.clone()),
                types::Pointer::new(result_type.clone()),
            )?;
            builder.store(
                build::variable("result", result_type.clone()),
                result_pointer,
            );

            builder.return_(Undefined::new(context.result_type.clone()))
        },
        context.result_type.clone(),
        CallingConvention::Tail,
        Linkage::Internal,
    ));

    Ok(build::variable(name, result_type.clone()))
}
